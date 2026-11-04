//! Versioned, bounded storage. The old claims file remains untouched after migration.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use crate::model::{
    Catalog, Claim, Entry, Error, Kit, MAX_CLAIMS, MAX_ITEM_BYTES, MAX_KITS, PlayerId,
    PlayerRecord, Stack, player_id,
};

pub const FILE: &str = "kits-v2.bin";
pub const MAX_FILE: usize = 12 * 1024 * 1024;
const HEADER: &[u8] = b"KITS\x02\x00\x00\x00";

pub fn encode(catalog: &Catalog) -> Result<Vec<u8>, Error> {
    catalog.validate()?;
    let mut out = HEADER.to_vec();
    put_u64(&mut out, catalog.next_id);
    put_u64(&mut out, catalog.next_operation);
    put_u64(&mut out, catalog.first_join.unwrap_or(0));
    put_u32(&mut out, catalog.kits.len());
    for kit in &catalog.kits {
        put_u64(&mut out, kit.id);
        put_bytes(&mut out, kit.name.as_bytes());
        put_u64(&mut out, kit.cooldown);
        out.push(
            u8::from(kit.one_time) | (u8::from(kit.restricted) << 1) | (u8::from(kit.listed) << 2),
        );
        put_u32(&mut out, kit.entries.len());
        for entry in &kit.entries {
            out.extend_from_slice(&[entry.slot, entry.stack.count]);
            put_bytes(&mut out, entry.stack.key.as_bytes());
            put_bytes(&mut out, &entry.stack.data);
        }
    }
    put_u32(&mut out, catalog.claims.len());
    for ((player, kit), claim) in &catalog.claims {
        out.extend_from_slice(player);
        put_u64(&mut out, *kit);
        put_u64(&mut out, claim.at);
        put_u64(&mut out, claim.operation);
        out.push(u8::from(claim.pending));
    }
    if out.len() > MAX_FILE {
        return Err(Error::Limit);
    }
    Ok(out)
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) {
    put_u32(out, value.len());
    out.extend_from_slice(value);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let result = self.0.get(..count).ok_or(Error::Storage)?;
        self.0 = &self.0[count..];
        Ok(result)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Error::Storage)?,
        ))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, Error> {
        let value =
            u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| Error::Storage)?) as usize;
        if value > maximum {
            return Err(Error::Storage);
        }
        Ok(value)
    }

    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], Error> {
        let count = self.count(maximum)?;
        self.take(count)
    }

    fn string(&mut self, maximum: usize) -> Result<String, Error> {
        String::from_utf8(self.bytes(maximum)?.to_vec()).map_err(|_| Error::Storage)
    }
}

pub fn decode(bytes: &[u8]) -> Result<Catalog, Error> {
    if bytes.len() > MAX_FILE {
        return Err(Error::Storage);
    }
    let mut input = Reader(bytes);
    if input.take(HEADER.len())? != HEADER {
        return Err(Error::Storage);
    }
    let next_id = input.u64()?;
    let next_operation = input.u64()?;
    let first = input.u64()?;
    let mut catalog = Catalog {
        next_id,
        next_operation,
        first_join: (first != 0).then_some(first),
        kits: Vec::new(),
        claims: Default::default(),
    };
    for _ in 0..input.count(MAX_KITS)? {
        let id = input.u64()?;
        let name = input.string(32)?;
        let cooldown = input.u64()?;
        let flags = input.byte()?;
        if flags > 7 {
            return Err(Error::Storage);
        }
        let mut entries = Vec::new();
        for _ in 0..input.count(41)? {
            let slot = input.byte()?;
            let count = input.byte()?;
            let key = input.string(128)?;
            let data = input.bytes(MAX_ITEM_BYTES)?.to_vec();
            entries.push(Entry {
                slot,
                stack: Stack { key, count, data },
            });
        }
        catalog.kits.push(Kit {
            id,
            name,
            cooldown,
            one_time: flags & 1 != 0,
            restricted: flags & 2 != 0,
            listed: flags & 4 != 0,
            entries,
        });
    }
    for _ in 0..input.count(MAX_CLAIMS)? {
        let player = input.take(16)?.try_into().map_err(|_| Error::Storage)?;
        let kit = input.u64()?;
        let at = input.u64()?;
        let operation = input.u64()?;
        let pending = input.byte()?;
        if pending > 1
            || catalog
                .claims
                .insert(
                    (player, kit),
                    Claim {
                        at,
                        operation,
                        pending: pending == 1,
                    },
                )
                .is_some()
        {
            return Err(Error::Storage);
        }
    }
    if !input.0.is_empty() {
        return Err(Error::Storage);
    }
    catalog.validate().map_err(|_| Error::Storage)?;
    Ok(catalog)
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    read_bounded(path, MAX_FILE)
}

pub fn read_bounded(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, Error> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Storage),
    };
    if file.metadata().map_err(|_| Error::Storage)?.len() > limit as u64 {
        return Err(Error::Storage);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Storage)?;
    if bytes.len() > limit {
        return Err(Error::Storage);
    }
    Ok(Some(bytes))
}

pub fn load(dir: &Path) -> Result<Catalog, Error> {
    if let Some(bytes) = read(&dir.join(FILE))? {
        return decode(&bytes);
    }
    let mut catalog = Catalog::default();
    if let Some(bytes) = read(&dir.join("claims.txt"))? {
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::Storage)?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split('\t');
            let player =
                player_id(fields.next().ok_or(Error::Storage)?).map_err(|_| Error::Storage)?;
            let name = fields.next().ok_or(Error::Storage)?;
            let at = fields
                .next()
                .ok_or(Error::Storage)?
                .parse::<u64>()
                .map_err(|_| Error::Storage)?;
            if fields.next().is_some() || catalog.claims.len() >= MAX_CLAIMS {
                return Err(Error::Storage);
            }
            let id = catalog.kit(name).map_err(|_| Error::Storage)?.id;
            let operation = catalog.next_operation;
            if catalog
                .claims
                .insert(
                    (player, id),
                    Claim {
                        at,
                        operation,
                        pending: false,
                    },
                )
                .is_some()
            {
                return Err(Error::Storage);
            }
            catalog.next_operation += 1;
        }
    }
    catalog.validate().map_err(|_| Error::Storage)?;
    Ok(catalog)
}

pub fn save(dir: &Path, catalog: &Catalog) -> Result<(), Error> {
    let bytes = encode(catalog)?;
    let temporary = dir.join("kits-v2.tmp");
    let mut file = fs::File::create(&temporary).map_err(|_| Error::Storage)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| Error::Storage)?;
    drop(file);
    fs::rename(&temporary, dir.join(FILE)).map_err(|_| Error::Storage)
}

fn player_file(dir: &Path, player: PlayerId) -> std::path::PathBuf {
    let name: String = player.iter().map(|byte| format!("{byte:02x}")).collect();
    dir.join("players").join(format!("{name}.bin"))
}

pub fn load_player(dir: &Path, player: PlayerId) -> Result<PlayerRecord, Error> {
    let Some(bytes) = read_bounded(&player_file(dir, player), 4096)? else {
        return Ok(PlayerRecord::default());
    };
    let mut input = Reader(&bytes);
    if input.take(5)? != b"KPLY\x01" || input.take(16)? != player {
        return Err(Error::Storage);
    }
    let seen = input.byte()?;
    if seen > 1 {
        return Err(Error::Storage);
    }
    let mut record = PlayerRecord {
        seen: seen == 1,
        uses: Default::default(),
    };
    for _ in 0..input.count(MAX_KITS)? {
        let id = input.u64()?;
        let at = input.u64()?;
        if id == 0 || record.uses.insert(id, at).is_some() {
            return Err(Error::Storage);
        }
    }
    if !input.0.is_empty() {
        return Err(Error::Storage);
    }
    Ok(record)
}

pub fn save_player(dir: &Path, player: PlayerId, record: &PlayerRecord) -> Result<(), Error> {
    if record.uses.len() > MAX_KITS || record.uses.contains_key(&0) {
        return Err(Error::Limit);
    }
    let mut bytes = b"KPLY\x01".to_vec();
    bytes.extend_from_slice(&player);
    bytes.push(u8::from(record.seen));
    put_u32(&mut bytes, record.uses.len());
    for (id, at) in &record.uses {
        put_u64(&mut bytes, *id);
        put_u64(&mut bytes, *at);
    }
    fs::create_dir_all(dir.join("players")).map_err(|_| Error::Storage)?;
    let path = player_file(dir, player);
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary).map_err(|_| Error::Storage)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| Error::Storage)?;
    drop(file);
    fs::rename(&temporary, &path).map_err(|_| Error::Storage)
}

pub const IMPORT_FILE: &str = "import-uses.txt";
const MAX_IMPORT_FILE: usize = 8 * 1024 * 1024;
const MAX_IMPORT_LINES: usize = 65_536;

/// `unit=seconds` or `unit=milliseconds`, then `<player uuid> <kit> <time>` lines. One bad line
/// refuses the whole file, before anything is written.
fn parse_uses(
    text: &str,
    catalog: &Catalog,
    now: u64,
) -> Result<std::collections::BTreeMap<PlayerId, std::collections::BTreeMap<u64, u64>>, Error> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let divisor = match lines.next() {
        Some("unit=seconds") => 1,
        Some("unit=milliseconds") => 1000,
        _ => return Err(Error::Invalid),
    };
    let latest = now.saturating_add(crate::model::MAX_COOLDOWN);
    let mut out = std::collections::BTreeMap::<PlayerId, std::collections::BTreeMap<_, _>>::new();
    for (index, line) in lines.enumerate() {
        if index >= MAX_IMPORT_LINES {
            return Err(Error::Limit);
        }
        let mut fields = line.split_ascii_whitespace();
        let player = player_id(fields.next().ok_or(Error::Invalid)?)?;
        let kit = catalog.kit(fields.next().ok_or(Error::Invalid)?)?.id;
        let time: u64 = fields
            .next()
            .ok_or(Error::Invalid)?
            .parse()
            .map_err(|_| Error::Invalid)?;
        // Rounded up: an imported cooldown is never shorter than the original.
        let at = time.div_ceil(divisor);
        if fields.next().is_some() || at == 0 || at > latest {
            return Err(Error::Invalid);
        }
        let last = out.entry(player).or_default().entry(kit).or_insert(at);
        *last = (*last).max(at);
    }
    Ok(out)
}

/// Returns how many player files changed. Running it again changes nothing, and a stored use is
/// never moved back.
pub fn import_uses(dir: &Path, catalog: &Catalog, now: u64) -> Result<usize, Error> {
    let bytes = read_bounded(&dir.join(IMPORT_FILE), MAX_IMPORT_FILE)?.ok_or(Error::NoImport)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    let mut changed = 0;
    for (player, uses) in parse_uses(text, catalog, now)? {
        let mut record = load_player(dir, player)?;
        let before = record.clone();
        record.seen = true;
        for (id, at) in uses {
            let last = record.uses.entry(id).or_insert(at);
            *last = (*last).max(at);
        }
        if record != before {
            save_player(dir, player, &record)?;
            changed += 1;
        }
    }
    Ok(changed)
}

pub fn initialize(dir: &Path) -> Result<Catalog, Error> {
    let mut catalog = load(dir)?;
    let active: std::collections::BTreeSet<_> = catalog.kits.iter().map(|kit| kit.id).collect();
    let mut records = std::collections::BTreeMap::<PlayerId, PlayerRecord>::new();
    for ((player, id), claim) in &catalog.claims {
        let record = records.entry(*player).or_default();
        record.seen = true;
        if active.contains(id) {
            record.uses.insert(*id, claim.at);
        }
    }
    for (player, legacy) in records {
        let mut record = load_player(dir, player)?;
        record.seen |= legacy.seen;
        for (id, at) in legacy.uses {
            let last = record.uses.entry(id).or_insert(at);
            *last = (*last).max(at);
        }
        record.uses.retain(|id, _| active.contains(id));
        save_player(dir, player, &record)?;
    }
    let needs_save = !catalog.claims.is_empty() || !dir.join(FILE).exists();
    catalog.claims.clear();
    if needs_save {
        save(dir, &catalog)?;
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{PLAYER, item};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn directory() -> std::path::PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!(
            "kits-lite-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn small_player_record_survives_restart_independently_of_catalog_changes() {
        let dir = directory();
        let catalog = Catalog::default();
        let mut record = PlayerRecord::default();
        record.record(&catalog, &catalog.kits[0], 100).unwrap();
        save_player(&dir, PLAYER, &record).unwrap();
        assert_eq!(load_player(&dir, PLAYER), Ok(record.clone()));
        let mut edited = catalog.clone();
        edited.kits[0].cooldown = 700;
        save(&dir, &edited).unwrap();
        assert_eq!(load_player(&dir, PLAYER), Ok(record));
    }

    #[test]
    fn failed_player_save_keeps_the_previous_use_record() {
        let dir = directory();
        let original = PlayerRecord {
            seen: true,
            uses: Default::default(),
        };
        save_player(&dir, PLAYER, &original).unwrap();
        fs::create_dir(player_file(&dir, PLAYER).with_extension("tmp")).unwrap();
        let mut changed = original.clone();
        let catalog = Catalog::default();
        changed.record(&catalog, &catalog.kits[0], 100).unwrap();
        assert_eq!(save_player(&dir, PLAYER, &changed), Err(Error::Storage));
        assert_eq!(load_player(&dir, PLAYER), Ok(original));
    }

    #[test]
    fn a_record_saved_before_delivery_is_not_replayed_after_restart() {
        let dir = directory();
        let mut catalog = Catalog::default();
        catalog.kits[0].one_time = true;
        let mut record = PlayerRecord::default();
        record.record(&catalog, &catalog.kits[0], 100).unwrap();
        save_player(&dir, PLAYER, &record).unwrap();
        let restarted = load_player(&dir, PLAYER).unwrap();
        assert_eq!(
            restarted.eligible(&catalog.kits[0], u64::MAX),
            Err(Error::Once)
        );
    }

    #[test]
    fn migration_moves_legacy_claims_to_player_records_without_resetting_a_use() {
        let dir = directory();
        fs::write(
            dir.join("claims.txt"),
            "07070707-0707-0707-0707-070707070707\tstarter\t100\n",
        )
        .unwrap();
        let catalog = initialize(&dir).unwrap();
        assert!(catalog.claims.is_empty());
        let record = load_player(&dir, PLAYER).unwrap();
        assert_eq!(
            record.eligible(catalog.kit("starter").unwrap(), 200),
            Err(Error::Cooldown(500))
        );
        assert_eq!(initialize(&dir), Ok(catalog));
        assert!(dir.join("claims.txt").exists());
    }

    #[test]
    fn restart_roundtrip_preserves_opaque_item_data_equipment_and_pending_claim() {
        let mut state = Catalog::default();
        let id = state
            .create(
                "daily",
                vec![item(0), item(36), item(39), item(40)],
                42,
                true,
            )
            .unwrap();
        state.first_join = Some(id);
        state.begin(PLAYER, "daily", 900).unwrap();
        let bytes = encode(&state).unwrap();
        assert_eq!(decode(&bytes), Ok(state.clone()));
        let dir = directory();
        save(&dir, &state).unwrap();
        assert_eq!(load(&dir), Ok(state));
    }

    #[test]
    fn truncated_unknown_version_oversized_and_trailing_bytes_are_refused() {
        let bytes = encode(&Catalog::default()).unwrap();
        for index in 0..bytes.len() {
            assert_eq!(decode(&bytes[..index]), Err(Error::Storage));
        }
        let mut bad = bytes.clone();
        bad[4] = 255;
        assert_eq!(decode(&bad), Err(Error::Storage));
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(Error::Storage));
        assert_eq!(decode(&vec![0; MAX_FILE + 1]), Err(Error::Storage));
    }

    #[test]
    fn legacy_cooldowns_migrate_once_and_the_original_file_is_preserved() {
        let dir = directory();
        let text = "07070707-0707-0707-0707-070707070707\tstarter\t100\n";
        fs::write(dir.join("claims.txt"), text).unwrap();
        let catalog = load(&dir).unwrap();
        assert_eq!(
            catalog.eligible(PLAYER, catalog.kit("starter").unwrap(), 200),
            Err(Error::Cooldown(500))
        );
        save(&dir, &catalog).unwrap();
        fs::write(
            dir.join("claims.txt"),
            "old file is not read after migration",
        )
        .unwrap();
        assert_eq!(load(&dir), Ok(catalog));
        assert!(dir.join("claims.txt").exists());
    }

    #[test]
    fn malformed_legacy_and_corrupt_current_data_never_reset_history() {
        let dir = directory();
        fs::write(dir.join("claims.txt"), "garbage\n").unwrap();
        assert_eq!(load(&dir), Err(Error::Storage));
        assert_eq!(
            fs::read_to_string(dir.join("claims.txt")).unwrap(),
            "garbage\n"
        );
        fs::write(dir.join(FILE), "bad").unwrap();
        assert_eq!(load(&dir), Err(Error::Storage));
        assert_eq!(fs::read_to_string(dir.join(FILE)).unwrap(), "bad");
    }

    #[test]
    fn failed_save_leaves_previous_committed_state_intact() {
        let dir = directory();
        let original = Catalog::default();
        save(&dir, &original).unwrap();
        fs::create_dir(dir.join("kits-v2.tmp")).unwrap();
        let mut edited = original.clone();
        edited.delete("tools").unwrap();
        assert_eq!(save(&dir, &edited), Err(Error::Storage));
        assert_eq!(load(&dir), Ok(original));
    }

    #[test]
    fn invalid_catalog_cannot_replace_a_good_file() {
        let dir = directory();
        let mut state = Catalog::default();
        save(&dir, &state).unwrap();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        state.kits.push(state.kits[0].clone());
        assert!(save(&dir, &state).is_err());
        assert_eq!(fs::read(dir.join(FILE)).unwrap(), bytes);
    }

    #[test]
    fn hostile_length_prefixes_do_not_allocate_the_requested_amount() {
        let mut bytes = encode(&Catalog::default()).unwrap();
        // Header + next-id + next-operation + first-join-id, then kit count.
        bytes[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(Error::Storage));
    }

    const OTHER: &str = "08080808-0808-0808-0808-080808080808";
    const SEVEN: &str = "07070707-0707-0707-0707-070707070707";

    #[test]
    fn listed_kits_survive_a_restart_as_listed() {
        let mut state = Catalog::default();
        state.kits[0].listed = true;
        assert_eq!(decode(&encode(&state).unwrap()), Ok(state));
    }

    #[test]
    fn importing_uses_is_repeatable_never_moves_a_use_back_and_keeps_other_kits() {
        let dir = directory();
        let catalog = Catalog::default();
        let (starter, tools) = (
            catalog.kit("starter").unwrap(),
            catalog.kit("tools").unwrap(),
        );
        let mut existing = PlayerRecord::default();
        existing.record(&catalog, tools, 5000).unwrap();
        save_player(&dir, PLAYER, &existing).unwrap();
        fs::write(
            dir.join(IMPORT_FILE),
            format!(
                "# from the old server\nunit=seconds\n{SEVEN} STARTER 4000\n{SEVEN} tools 100\n{OTHER} tools 4500\n"
            ),
        )
        .unwrap();
        assert_eq!(import_uses(&dir, &catalog, 6000), Ok(2));
        let record = load_player(&dir, PLAYER).unwrap();
        assert_eq!(record.uses[&starter.id], 4000);
        assert_eq!(
            record.uses[&tools.id], 5000,
            "an older import changes nothing"
        );
        assert_eq!(
            import_uses(&dir, &catalog, 6000),
            Ok(0),
            "a second run changes nothing"
        );
        assert_eq!(load_player(&dir, PLAYER), Ok(record));
    }

    #[test]
    fn imported_milliseconds_round_up_so_a_cooldown_never_shortens() {
        let dir = directory();
        let catalog = Catalog::default();
        fs::write(
            dir.join(IMPORT_FILE),
            format!("unit=milliseconds\n{SEVEN} starter 4000001\n"),
        )
        .unwrap();
        import_uses(&dir, &catalog, 6000).unwrap();
        let id = catalog.kit("starter").unwrap().id;
        assert_eq!(load_player(&dir, PLAYER).unwrap().uses[&id], 4001);
    }

    #[test]
    fn a_bad_import_file_changes_no_player_at_all() {
        let catalog = Catalog::default();
        let good = format!("{SEVEN} starter 100");
        for bad in [
            format!("{good}\n"),
            format!("unit=days\n{good}\n"),
            format!("unit=seconds\n{good}\nnot-a-uuid starter 100\n"),
            format!("unit=seconds\n{good}\n{OTHER} nope 100\n"),
            format!("unit=seconds\n{good}\n{OTHER} starter soon\n"),
            format!("unit=seconds\n{good}\n{OTHER} starter 0\n"),
            format!("unit=seconds\n{good}\n{OTHER} starter 100 extra\n"),
            format!("unit=seconds\n{good}\n{OTHER} starter 99999999999\n"),
        ] {
            let dir = directory();
            fs::write(dir.join(IMPORT_FILE), &bad).unwrap();
            assert!(import_uses(&dir, &catalog, 6000).is_err(), "{bad}");
            assert_eq!(load_player(&dir, PLAYER), Ok(PlayerRecord::default()));
        }
        assert_eq!(
            import_uses(&directory(), &catalog, 6000),
            Err(Error::NoImport)
        );
    }

    #[test]
    fn duplicate_legacy_records_are_not_silently_coalesced() {
        let dir = directory();
        let record = "07070707-0707-0707-0707-070707070707\tstarter\t100\n";
        fs::write(dir.join("claims.txt"), record.repeat(2)).unwrap();
        assert_eq!(load(&dir), Err(Error::Storage));
    }
}
