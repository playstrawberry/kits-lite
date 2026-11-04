//! Bounded kit definitions and claim eligibility, independent of host handles.

use std::collections::BTreeMap;

pub const MAX_KITS: usize = 128;
pub const MAX_CLAIMS: usize = 32_768;
pub const MAX_ITEM_BYTES: usize = 65_536;
pub const MAX_KIT_BYTES: usize = 524_288;
pub const MAX_CATALOG_BYTES: usize = 8_388_608;
pub const MAX_COOLDOWN: u64 = 31_536_000;
pub const SLOT_COUNT: usize = 41;
pub type PlayerId = [u8; 16];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unknown,
    Exists,
    Limit,
    Empty,
    Full,
    Busy,
    Cooldown(u64),
    Once,
    #[cfg(test)]
    Pending,
    Storage,
    Permission,
    PlayerOnly,
    Offline,
    /// The kit comes from the settings file; change it there.
    Defined,
    /// The server turned this feature off in the settings file.
    Disabled,
    /// A concurrent inventory change left no room for part of a delivery.
    Partial,
    /// There is no use-import file in the data folder.
    NoImport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    pub key: String,
    pub count: u8,
    /// Empty for built-in defaults; otherwise a versioned component snapshot.
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Main 0..35, boots 36, leggings 37, chestplate 38, helmet 39, offhand 40.
    pub slot: u8,
    pub stack: Stack,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kit {
    pub id: u64,
    pub name: String,
    pub cooldown: u64,
    pub one_time: bool,
    pub restricted: bool,
    /// Defined by readable lines: placed compactly and never equipped.
    pub listed: bool,
    pub entries: Vec<Entry>,
}

/// Component kind and returned bytes, sorted by kind.
pub type Components = Vec<(u16, Vec<u8>)>;

/// What one inventory slot holds, as the server reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    Empty,
    Item {
        key: String,
        count: u8,
        max: u8,
        components: Components,
    },
}

/// What the server reports for one kit line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTraits {
    pub max: u8,
    pub components: Components,
}

/// One write of a delivery: a new stack in an empty slot, or more items on a matching stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub entry: usize,
    pub slot: u8,
    pub count: u8,
    pub merge: bool,
}

/// The inventory a delivery reads and writes.
pub trait Slots {
    fn read(&self, slot: u8) -> Held;
    /// Returns false when the item could not be built; the slot is unchanged then.
    fn write(&mut self, piece: &Piece) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Given {
    All,
    /// Nothing was written and the kit no longer fits; the recorded use can be restored.
    Nothing,
    /// Some items were written and this many had no room left.
    Partial(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub at: u64,
    pub operation: u64,
    pub pending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerRecord {
    pub seen: bool,
    pub uses: BTreeMap<u64, u64>,
}

impl PlayerRecord {
    pub fn eligible(&self, kit: &Kit, now: u64) -> Result<(), Error> {
        if let Some(last) = self.uses.get(&kit.id) {
            if kit.one_time {
                return Err(Error::Once);
            }
            let left = last.saturating_add(kit.cooldown).saturating_sub(now);
            if left != 0 {
                return Err(Error::Cooldown(left));
            }
        }
        Ok(())
    }

    pub fn record(&mut self, catalog: &Catalog, kit: &Kit, now: u64) -> Result<(), Error> {
        self.eligible(kit, now)?;
        let active: std::collections::BTreeSet<_> = catalog.kits.iter().map(|kit| kit.id).collect();
        self.uses.retain(|id, _| active.contains(id));
        if self.uses.len() >= MAX_KITS && !self.uses.contains_key(&kit.id) {
            return Err(Error::Limit);
        }
        self.uses.insert(kit.id, now);
        self.seen = true;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub next_id: u64,
    pub next_operation: u64,
    pub first_join: Option<u64>,
    pub kits: Vec<Kit>,
    /// Imported older-format history only; initialization moves it to player records.
    pub claims: BTreeMap<(PlayerId, u64), Claim>,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        && ![
            "create",
            "edit",
            "delete",
            "give",
            "preview",
            "set",
            "firstjoin",
            "list",
        ]
        .contains(&name)
}

pub fn player_id(uuid: &str) -> Result<PlayerId, Error> {
    if uuid.len() != 36 {
        return Err(Error::Invalid);
    }
    let mut out = [0; 16];
    let mut digit = 0;
    for (offset, byte) in uuid.bytes().enumerate() {
        if [8, 13, 18, 23].contains(&offset) {
            if byte != b'-' {
                return Err(Error::Invalid);
            }
            continue;
        }
        let value = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return Err(Error::Invalid),
        };
        out[digit / 2] = (out[digit / 2] << 4) | value;
        digit += 1;
    }
    Ok(out)
}

pub fn validate_kit(kit: &Kit) -> Result<(), Error> {
    if kit.id == 0 || !valid_name(&kit.name) || !(1..=MAX_COOLDOWN).contains(&kit.cooldown) {
        return Err(Error::Invalid);
    }
    if kit.entries.is_empty() {
        return Err(Error::Empty);
    }
    if kit.entries.len() > SLOT_COUNT || kit_bytes(kit) > MAX_KIT_BYTES {
        return Err(Error::Limit);
    }
    let mut seen = 0u64;
    for entry in &kit.entries {
        let stack = &entry.stack;
        if usize::from(entry.slot) >= SLOT_COUNT
            || seen & (1u64 << entry.slot) != 0
            || stack.count == 0
            || stack.count > 99
            || stack.key.len() > 128
            || stack.key.split_once(':').is_none_or(|(namespace, key)| {
                namespace.is_empty()
                    || key.is_empty()
                    || !namespace.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c)
                    })
                    || !key.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-/".contains(&c)
                    })
            })
            || stack.key == "minecraft:air"
        {
            return Err(Error::Invalid);
        }
        if stack.data.len() > MAX_ITEM_BYTES {
            return Err(Error::Limit);
        }
        seen |= 1u64 << entry.slot;
    }
    Ok(())
}

pub fn kit_bytes(kit: &Kit) -> usize {
    kit.entries
        .iter()
        .map(|entry| entry.stack.data.len() + entry.stack.key.len() + 16)
        .sum::<usize>()
        + kit.name.len()
        + 32
}

/// Plan every write before recording a use or changing any inventory slot.
pub fn plan(
    kit: &Kit,
    traits: &[ItemTraits],
    held: &[Held; SLOT_COUNT],
) -> Result<Vec<Piece>, Error> {
    validate_kit(kit)?;
    if traits.len() != kit.entries.len() {
        return Err(Error::Invalid);
    }
    let mut after = held.clone();
    let mut out = Vec::with_capacity(kit.entries.len());
    for (index, entry) in kit.entries.iter().enumerate() {
        let desired = (!kit.listed).then_some(entry.slot);
        let stack = &entry.stack;
        let left = place(
            index,
            &stack.key,
            stack.count,
            desired,
            &traits[index],
            &mut after,
            &mut out,
        )?;
        if left != 0 {
            return Err(Error::Full);
        }
    }
    Ok(out)
}

/// Captured position first when it is empty, then matching partial stacks, then the lowest empty
/// main slots. Returns how many items had no room.
fn place(
    entry: usize,
    key: &str,
    mut left: u8,
    desired: Option<u8>,
    traits: &ItemTraits,
    after: &mut [Held; SLOT_COUNT],
    out: &mut Vec<Piece>,
) -> Result<u8, Error> {
    let max = traits.max;
    if max == 0 {
        return Err(Error::Invalid);
    }
    if let Some(slot) = desired
        && left <= max
        && after.get(usize::from(slot)) == Some(&Held::Empty)
    {
        fill(entry, key, slot, left, traits, after, out);
        return Ok(0);
    }
    for slot in 0..36u8 {
        if left == 0 {
            return Ok(0);
        }
        if let Held::Item {
            key: held_key,
            count,
            max: held_max,
            components,
        } = &mut after[usize::from(slot)]
            && max > 1
            && *held_max == max
            && *count < max
            && held_key == key
            && *components == traits.components
        {
            let moved = left.min(max - *count);
            *count += moved;
            left -= moved;
            out.push(Piece {
                entry,
                slot,
                count: moved,
                merge: true,
            });
        }
    }
    for slot in 0..36u8 {
        if left == 0 {
            return Ok(0);
        }
        if after[usize::from(slot)] == Held::Empty {
            let moved = left.min(max);
            left -= moved;
            fill(entry, key, slot, moved, traits, after, out);
        }
    }
    Ok(left)
}

fn fill(
    entry: usize,
    key: &str,
    slot: u8,
    count: u8,
    traits: &ItemTraits,
    after: &mut [Held; SLOT_COUNT],
    out: &mut Vec<Piece>,
) {
    after[usize::from(slot)] = Held::Item {
        key: key.into(),
        count,
        max: traits.max,
        components: traits.components.clone(),
    };
    out.push(Piece {
        entry,
        slot,
        count,
        merge: false,
    });
}

const MAX_REPLANS: u32 = 3;

/// Writes a planned kit after its use was recorded. A slot that changed since it was read is never
/// overwritten: what is left is placed again from a fresh read.
pub fn deliver<S: Slots>(
    kit: &Kit,
    traits: &[ItemTraits],
    planned_from: &[Held; SLOT_COUNT],
    planned: Vec<Piece>,
    slots: &mut S,
) -> Given {
    let mut current = read_all(slots);
    let mut pieces = if current == *planned_from {
        planned
    } else {
        match plan(kit, traits, &current) {
            Ok(pieces) => pieces,
            Err(_) => return Given::Nothing,
        }
    };
    let (mut next, mut replans, mut written, mut lost) = (0, 0, false, 0u32);
    while let Some(piece) = pieces.get(next).cloned() {
        if slots.read(piece.slot) != current[usize::from(piece.slot)] {
            replans += 1;
            current = read_all(slots);
            let mut after = current.clone();
            let mut again = Vec::new();
            for rest in &pieces[next..] {
                if replans > MAX_REPLANS {
                    lost += u32::from(rest.count);
                    continue;
                }
                let key = &kit.entries[rest.entry].stack.key;
                let line = &traits[rest.entry];
                match place(
                    rest.entry, key, rest.count, None, line, &mut after, &mut again,
                ) {
                    Ok(left) => lost += u32::from(left),
                    Err(_) => lost += u32::from(rest.count),
                }
            }
            if lost != 0 && !written {
                return Given::Nothing;
            }
            pieces = again;
            next = 0;
            continue;
        }
        if slots.write(&piece) {
            written = true;
            apply_piece(&mut current, kit, traits, &piece);
        } else {
            lost += u32::from(piece.count);
        }
        next += 1;
    }
    match (lost, written) {
        (0, _) => Given::All,
        (_, false) => Given::Nothing,
        (lost, true) => Given::Partial(lost),
    }
}

fn read_all<S: Slots>(slots: &S) -> [Held; SLOT_COUNT] {
    std::array::from_fn(|slot| slots.read(slot as u8))
}

fn apply_piece(current: &mut [Held; SLOT_COUNT], kit: &Kit, traits: &[ItemTraits], piece: &Piece) {
    let held = &mut current[usize::from(piece.slot)];
    if let (true, Held::Item { count, .. }) = (piece.merge, &mut *held) {
        *count = count.saturating_add(piece.count);
    } else {
        *held = Held::Item {
            key: kit.entries[piece.entry].stack.key.clone(),
            count: piece.count,
            max: traits[piece.entry].max,
            components: traits[piece.entry].components.clone(),
        };
    }
}

impl Default for Catalog {
    fn default() -> Self {
        let defaults = |id, name: &str, cooldown, items: &[(&str, u8)]| Kit {
            id,
            name: name.into(),
            cooldown,
            one_time: false,
            restricted: false,
            listed: false,
            entries: items
                .iter()
                .enumerate()
                .map(|(slot, (key, count))| Entry {
                    slot: slot as u8,
                    stack: Stack {
                        key: (*key).into(),
                        count: *count,
                        data: Vec::new(),
                    },
                })
                .collect(),
        };
        Self {
            next_id: 3,
            next_operation: 1,
            first_join: None,
            kits: vec![
                defaults(
                    1,
                    "starter",
                    600,
                    &[
                        ("minecraft:bread", 16),
                        ("minecraft:stone_pickaxe", 1),
                        ("minecraft:stone_axe", 1),
                        ("minecraft:torch", 16),
                    ],
                ),
                defaults(
                    2,
                    "tools",
                    3600,
                    &[
                        ("minecraft:iron_pickaxe", 1),
                        ("minecraft:iron_axe", 1),
                        ("minecraft:iron_shovel", 1),
                    ],
                ),
            ],
            claims: BTreeMap::new(),
        }
    }
}

impl Catalog {
    pub fn kit(&self, name: &str) -> Result<&Kit, Error> {
        self.kits
            .iter()
            .find(|kit| kit.name.eq_ignore_ascii_case(name))
            .ok_or(Error::Unknown)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.kits.len() > MAX_KITS
            || self.claims.len() > MAX_CLAIMS
            || self.kits.iter().map(kit_bytes).sum::<usize>() > MAX_CATALOG_BYTES
        {
            return Err(Error::Limit);
        }
        if self.next_id == 0 || self.next_operation == 0 {
            return Err(Error::Invalid);
        }
        let mut names = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut operations = std::collections::BTreeSet::new();
        for kit in &self.kits {
            validate_kit(kit)?;
            if kit.id >= self.next_id || !ids.insert(kit.id) || !names.insert(&kit.name) {
                return Err(Error::Invalid);
            }
        }
        if self.first_join.is_some_and(|id| !ids.contains(&id)) {
            return Err(Error::Invalid);
        }
        for ((_, id), claim) in &self.claims {
            if *id == 0
                || *id >= self.next_id
                || claim.operation == 0
                || claim.operation >= self.next_operation
                || !operations.insert(claim.operation)
            {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }

    pub fn create(
        &mut self,
        name: &str,
        entries: Vec<Entry>,
        cooldown: u64,
        once: bool,
    ) -> Result<u64, Error> {
        if self
            .kits
            .iter()
            .any(|kit| kit.name.eq_ignore_ascii_case(name))
        {
            return Err(Error::Exists);
        }
        if self.kits.len() >= MAX_KITS {
            return Err(Error::Limit);
        }
        let id = self.next_id;
        let next = id.checked_add(1).ok_or(Error::Limit)?;
        let kit = Kit {
            id,
            name: name.into(),
            cooldown,
            one_time: once,
            restricted: true,
            listed: false,
            entries,
        };
        validate_kit(&kit)?;
        if self.kits.iter().map(kit_bytes).sum::<usize>() + kit_bytes(&kit) > MAX_CATALOG_BYTES {
            return Err(Error::Limit);
        }
        self.kits.push(kit);
        self.next_id = next;
        Ok(id)
    }

    pub fn edit(&mut self, name: &str, entries: Vec<Entry>) -> Result<(), Error> {
        let index = self
            .kits
            .iter()
            .position(|kit| kit.name.eq_ignore_ascii_case(name))
            .ok_or(Error::Unknown)?;
        let mut kit = self.kits[index].clone();
        kit.entries = entries;
        validate_kit(&kit)?;
        if self
            .kits
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, k)| kit_bytes(k))
            .sum::<usize>()
            + kit_bytes(&kit)
            > MAX_CATALOG_BYTES
        {
            return Err(Error::Limit);
        }
        self.kits[index] = kit;
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), Error> {
        let index = self
            .kits
            .iter()
            .position(|kit| kit.name.eq_ignore_ascii_case(name))
            .ok_or(Error::Unknown)?;
        let removed = self.kits.remove(index);
        if self.first_join == Some(removed.id) {
            self.first_join = None;
        }
        Ok(())
    }

    /// Forgets every use of the named kits (all when `None`) by giving them new identities. Player
    /// files are not touched: their old entries no longer match and are pruned at the next use.
    pub fn renew_uses(&mut self, name: Option<&str>) -> Result<(), Error> {
        if let Some(name) = name {
            self.kit(name)?;
        }
        let mut next = self.next_id;
        let mut first_join = self.first_join;
        for kit in self
            .kits
            .iter_mut()
            .filter(|kit| name.is_none_or(|name| kit.name.eq_ignore_ascii_case(name)))
        {
            if first_join == Some(kit.id) {
                first_join = Some(next);
            }
            kit.id = next;
            next = next.checked_add(1).ok_or(Error::Limit)?;
        }
        self.next_id = next;
        self.first_join = first_join;
        Ok(())
    }

    #[cfg(test)]
    pub fn eligible(&self, player: PlayerId, kit: &Kit, now: u64) -> Result<(), Error> {
        if let Some(claim) = self.claims.get(&(player, kit.id)) {
            if claim.pending {
                return Err(Error::Pending);
            }
            if kit.one_time {
                return Err(Error::Once);
            }
            let left = claim.at.saturating_add(kit.cooldown).saturating_sub(now);
            if left > 0 {
                return Err(Error::Cooldown(left));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn begin(&mut self, player: PlayerId, name: &str, now: u64) -> Result<u64, Error> {
        let kit = self.kit(name)?;
        self.eligible(player, kit, now)?;
        let key = (player, kit.id);
        if !self.claims.contains_key(&key) && self.claims.len() >= MAX_CLAIMS {
            return Err(Error::Limit);
        }
        let operation = self.next_operation;
        let next = operation.checked_add(1).ok_or(Error::Limit)?;
        self.claims.insert(
            key,
            Claim {
                at: now,
                operation,
                pending: true,
            },
        );
        self.next_operation = next;
        Ok(operation)
    }

    #[cfg(test)]
    pub fn complete(&mut self, player: PlayerId, name: &str, operation: u64) -> Result<(), Error> {
        let key = (player, self.kit(name)?.id);
        let claim = self.claims.get_mut(&key).ok_or(Error::Invalid)?;
        if claim.operation != operation {
            return Err(Error::Invalid);
        }
        claim.pending = false;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PLAYER: PlayerId = [7; 16];

    fn stone() -> Held {
        Held::Item {
            key: "minecraft:stone".into(),
            count: 64,
            max: 64,
            components: Vec::new(),
        }
    }

    fn held(key: &str, count: u8, max: u8) -> Held {
        Held::Item {
            key: key.into(),
            count,
            max,
            components: Vec::new(),
        }
    }

    fn empty() -> [Held; SLOT_COUNT] {
        std::array::from_fn(|_| Held::Empty)
    }

    fn unstackable(kit: &Kit) -> Vec<ItemTraits> {
        vec![
            ItemTraits {
                max: 1,
                components: Vec::new(),
            };
            kit.entries.len()
        ]
    }

    fn occupied(slots: &[bool; SLOT_COUNT]) -> [Held; SLOT_COUNT] {
        std::array::from_fn(|slot| if slots[slot] { stone() } else { Held::Empty })
    }

    fn listed(lines: &[(&str, u8)]) -> Kit {
        Kit {
            id: 9,
            name: "listed".into(),
            cooldown: 10,
            one_time: false,
            restricted: true,
            listed: true,
            entries: lines
                .iter()
                .enumerate()
                .map(|(slot, (key, count))| Entry {
                    slot: slot as u8,
                    stack: Stack {
                        key: (*key).into(),
                        count: *count,
                        data: Vec::new(),
                    },
                })
                .collect(),
        }
    }

    /// Plain items: unstackable swords and totems, everything else stacks to 64.
    fn plain(kit: &Kit) -> Vec<ItemTraits> {
        kit.entries
            .iter()
            .map(|entry| ItemTraits {
                max: if entry.stack.key.contains("sword") || entry.stack.key.contains("totem") {
                    1
                } else {
                    64
                },
                components: Vec::new(),
            })
            .collect()
    }

    fn spots(pieces: &[Piece]) -> Vec<(u8, u8, bool)> {
        pieces
            .iter()
            .map(|piece| (piece.slot, piece.count, piece.merge))
            .collect()
    }

    #[test]
    fn matching_partial_stacks_are_filled_first_then_lowest_empty_slots_in_line_order() {
        let kit = listed(&[("minecraft:stone_sword", 1), ("minecraft:iron_ingot", 32)]);
        let mut slots = empty();
        slots[0] = stone();
        slots[5] = held("minecraft:iron_ingot", 50, 64);
        let pieces = plan(&kit, &plain(&kit), &slots).unwrap();
        assert_eq!(
            spots(&pieces),
            vec![(1, 1, false), (5, 14, true), (2, 18, false)]
        );
    }

    #[test]
    fn a_kit_fits_a_full_inventory_when_partial_stacks_can_take_it() {
        let kit = listed(&[("minecraft:iron_ingot", 32)]);
        let mut slots: [Held; SLOT_COUNT] = std::array::from_fn(|_| stone());
        slots[35] = held("minecraft:iron_ingot", 32, 64);
        assert_eq!(
            spots(&plan(&kit, &plain(&kit), &slots).unwrap()),
            vec![(35, 32, true)]
        );
        slots[35] = held("minecraft:iron_ingot", 33, 64);
        assert_eq!(plan(&kit, &plain(&kit), &slots), Err(Error::Full));
    }

    #[test]
    fn counts_above_the_item_maximum_split_into_legal_stacks() {
        let kit = listed(&[("minecraft:totem_of_undying", 2), ("minecraft:bread", 70)]);
        assert_eq!(
            spots(&plan(&kit, &plain(&kit), &empty()).unwrap()),
            vec![(0, 1, false), (1, 1, false), (2, 64, false), (3, 6, false)]
        );
    }

    #[test]
    fn stacks_with_other_components_or_another_maximum_never_merge() {
        let kit = listed(&[("minecraft:bread", 1)]);
        let mut slots = empty();
        slots[0] = Held::Item {
            key: "minecraft:bread".into(),
            count: 1,
            max: 64,
            components: vec![(6, vec![1, 2])],
        };
        slots[1] = held("minecraft:bread", 1, 16);
        assert_eq!(
            spots(&plan(&kit, &plain(&kit), &slots).unwrap()),
            vec![(2, 1, false)]
        );
    }

    #[test]
    fn unstackable_items_never_merge_and_listed_kits_never_equip() {
        let kit = listed(&[("minecraft:stone_sword", 1)]);
        let mut slots = empty();
        slots[0] = held("minecraft:stone_sword", 1, 1);
        let pieces = plan(&kit, &plain(&kit), &slots).unwrap();
        assert_eq!(spots(&pieces), vec![(1, 1, false)]);
        let mut line = listed(&[("minecraft:diamond_helmet", 1)]);
        line.entries[0].slot = 39;
        assert_eq!(
            spots(&plan(&line, &plain(&line), &empty()).unwrap()),
            vec![(0, 1, false)]
        );
    }

    struct Fake {
        slots: [Held; SLOT_COUNT],
        kit: Kit,
        traits: Vec<ItemTraits>,
        /// After this many writes, the given slot is taken by another item.
        intrude: Option<(usize, u8)>,
        writes: usize,
    }

    impl Slots for Fake {
        fn read(&self, slot: u8) -> Held {
            self.slots[usize::from(slot)].clone()
        }
        fn write(&mut self, piece: &Piece) -> bool {
            let held = &mut self.slots[usize::from(piece.slot)];
            match (&mut *held, piece.merge) {
                (Held::Item { count, .. }, true) => *count += piece.count,
                (Held::Empty, false) => {
                    *held = Held::Item {
                        key: self.kit.entries[piece.entry].stack.key.clone(),
                        count: piece.count,
                        max: self.traits[piece.entry].max,
                        components: Vec::new(),
                    }
                }
                other => panic!("overwrote an item: {other:?}"),
            }
            self.writes += 1;
            if let Some((after, slot)) = self.intrude
                && after == self.writes
            {
                self.slots[usize::from(slot)] = stone();
                self.intrude = None;
            }
            true
        }
    }

    fn fake(kit: &Kit, slots: [Held; SLOT_COUNT], intrude: Option<(usize, u8)>) -> Fake {
        Fake {
            slots,
            kit: kit.clone(),
            traits: plain(kit),
            intrude,
            writes: 0,
        }
    }

    fn total(slots: &[Held; SLOT_COUNT], key: &str) -> u32 {
        slots
            .iter()
            .map(|held| match held {
                Held::Item { key: k, count, .. } if k == key => u32::from(*count),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn delivery_writes_every_planned_item_once() {
        let kit = listed(&[("minecraft:bread", 70), ("minecraft:stone_sword", 1)]);
        let traits = plain(&kit);
        let start = empty();
        let pieces = plan(&kit, &traits, &start).unwrap();
        let mut slots = fake(&kit, start.clone(), None);
        assert_eq!(
            deliver(&kit, &traits, &start, pieces, &mut slots),
            Given::All
        );
        assert_eq!(total(&slots.slots, "minecraft:bread"), 70);
        assert_eq!(total(&slots.slots, "minecraft:stone_sword"), 1);
    }

    #[test]
    fn a_slot_taken_during_delivery_is_never_overwritten_and_nothing_is_duplicated() {
        let kit = listed(&[("minecraft:stone_sword", 1), ("minecraft:bread", 70)]);
        let traits = plain(&kit);
        let start = empty();
        let pieces = plan(&kit, &traits, &start).unwrap();
        // Slot 1 was planned for bread; another item lands there after the first write.
        let mut slots = fake(&kit, start.clone(), Some((1, 1)));
        assert_eq!(
            deliver(&kit, &traits, &start, pieces, &mut slots),
            Given::All
        );
        assert_eq!(slots.slots[1], stone());
        assert_eq!(total(&slots.slots, "minecraft:stone_sword"), 1);
        assert_eq!(total(&slots.slots, "minecraft:bread"), 70);
    }

    #[test]
    fn an_inventory_changed_after_planning_is_planned_again_or_gives_nothing() {
        let kit = listed(&[("minecraft:stone_sword", 1)]);
        let traits = plain(&kit);
        let start = empty();
        let pieces = plan(&kit, &traits, &start).unwrap();
        let mut changed = empty();
        changed[0] = stone();
        let mut slots = fake(&kit, changed, None);
        assert_eq!(
            deliver(&kit, &traits, &start, pieces.clone(), &mut slots),
            Given::All
        );
        assert_eq!(slots.slots[0], stone());
        assert_eq!(slots.slots[1], held("minecraft:stone_sword", 1, 1));

        let full: [Held; SLOT_COUNT] = std::array::from_fn(|_| stone());
        let mut slots = fake(&kit, full.clone(), None);
        assert_eq!(
            deliver(&kit, &traits, &start, pieces, &mut slots),
            Given::Nothing
        );
        assert_eq!(slots.slots, full);
    }

    #[test]
    fn room_lost_during_delivery_keeps_what_was_written_and_counts_the_rest() {
        let kit = listed(&[("minecraft:stone_sword", 1), ("minecraft:bread", 64)]);
        let traits = plain(&kit);
        let mut start: [Held; SLOT_COUNT] = std::array::from_fn(|_| stone());
        start[0] = Held::Empty;
        start[1] = Held::Empty;
        let pieces = plan(&kit, &traits, &start).unwrap();
        let mut slots = fake(&kit, start.clone(), Some((1, 1)));
        assert_eq!(
            deliver(&kit, &traits, &start, pieces, &mut slots),
            Given::Partial(64)
        );
        assert_eq!(total(&slots.slots, "minecraft:stone_sword"), 1);
        assert_eq!(total(&slots.slots, "minecraft:bread"), 0);
    }

    #[test]
    fn lookup_folds_ascii_only_and_never_by_locale() {
        let mut state = Catalog::default();
        state.create("vip", vec![item(0)], 10, false).unwrap();
        assert_eq!(state.kit("VIP").unwrap().name, "vip");
        assert_eq!(state.kit("Vip").unwrap().name, "vip");
        // Turkish dotted capital and dotless small i are other letters, not "i".
        assert_eq!(state.kit("V\u{130}P"), Err(Error::Unknown));
        assert_eq!(state.kit("v\u{131}p"), Err(Error::Unknown));
        assert_eq!(
            state.create("VIP", vec![item(0)], 10, false),
            Err(Error::Exists)
        );
    }

    #[test]
    fn renewing_uses_forgets_one_time_and_cooldown_history_but_not_seen_or_contents() {
        let mut state = Catalog::default();
        let id = state.create("once", vec![item(0)], 10, true).unwrap();
        state.first_join = Some(id);
        let mut record = PlayerRecord::default();
        let kit = state.kit("once").unwrap().clone();
        record.record(&state, &kit, 100).unwrap();
        assert_eq!(record.eligible(&kit, 200), Err(Error::Once));
        state.renew_uses(Some("ONCE")).unwrap();
        let renewed = state.kit("once").unwrap();
        assert_ne!(renewed.id, id);
        assert_eq!(renewed.entries, kit.entries);
        assert_eq!(state.first_join, Some(renewed.id));
        assert_eq!(record.eligible(renewed, 200), Ok(()));
        assert!(record.seen);
        assert_eq!(state.validate(), Ok(()));
        assert_eq!(state.renew_uses(Some("nope")), Err(Error::Unknown));
        let before = state.kit("starter").unwrap().id;
        state.renew_uses(None).unwrap();
        assert_ne!(state.kit("starter").unwrap().id, before);
        assert_eq!(state.validate(), Ok(()));
    }

    pub fn item(slot: u8) -> Entry {
        Entry {
            slot,
            stack: Stack {
                key: "minecraft:diamond_sword".into(),
                count: 1,
                data: vec![0, 255, 12, 1, 0],
            },
        }
    }

    #[test]
    fn durable_record_blocks_repeated_clicks_and_restart_without_any_pending_replay() {
        let catalog = Catalog::default();
        let kit = catalog.kit("starter").unwrap();
        let mut record = PlayerRecord::default();
        record.record(&catalog, kit, 100).unwrap();
        assert_eq!(record.eligible(kit, 101), Err(Error::Cooldown(599)));
        assert_eq!(record.clone().eligible(kit, 200), Err(Error::Cooldown(500)));
        assert_eq!(record.eligible(kit, 90), Err(Error::Cooldown(610)));
        assert_eq!(record.eligible(kit, 700), Ok(()));
    }

    #[test]
    fn one_time_is_kept_after_edit_and_seen_is_explicitly_recorded() {
        let mut catalog = Catalog::default();
        catalog.kits[0].one_time = true;
        let mut record = PlayerRecord::default();
        assert!(!record.seen);
        record.record(&catalog, &catalog.kits[0], 100).unwrap();
        assert!(record.seen);
        catalog.edit("starter", vec![item(39)]).unwrap();
        assert_eq!(record.eligible(&catalog.kits[0], 9000), Err(Error::Once));
        assert_eq!(Catalog::default().first_join, None);
    }

    #[test]
    fn inventory_kit_create_edit_delete_keeps_identity_and_history() {
        let mut state = Catalog::default();
        let id = state
            .create("daily", vec![item(0), item(39)], 10, false)
            .unwrap();
        state.begin(PLAYER, "daily", 100).unwrap();
        state.edit("daily", vec![item(1)]).unwrap();
        assert_eq!(state.kit("daily").unwrap().id, id);
        assert_eq!(
            state.eligible(PLAYER, state.kit("daily").unwrap(), 102),
            Err(Error::Pending)
        );
        state.first_join = Some(id);
        state.delete("daily").unwrap();
        assert_eq!(state.first_join, None);
        let next = state.create("daily", vec![item(2)], 10, false).unwrap();
        assert!(next > id);
        assert!(state.claims.contains_key(&(PLAYER, id)));
    }

    #[test]
    fn cooldown_boundaries_and_clock_rollback_are_conservative() {
        let mut state = Catalog::default();
        state.create("daily", vec![item(0)], 10, false).unwrap();
        let op = state.begin(PLAYER, "daily", 100).unwrap();
        state.complete(PLAYER, "daily", op).unwrap();
        let kit = state.kit("daily").unwrap();
        assert_eq!(state.eligible(PLAYER, kit, 109), Err(Error::Cooldown(1)));
        assert_eq!(state.eligible(PLAYER, kit, 90), Err(Error::Cooldown(20)));
        assert_eq!(state.eligible(PLAYER, kit, 110), Ok(()));
    }

    #[test]
    fn one_time_cannot_be_reset_by_editing_contents_or_restarting() {
        let mut state = Catalog::default();
        state.create("once", vec![item(0)], 1, true).unwrap();
        let op = state.begin(PLAYER, "once", 100).unwrap();
        state.complete(PLAYER, "once", op).unwrap();
        state.edit("once", vec![item(4)]).unwrap();
        let restarted = state.clone();
        assert_eq!(
            restarted.eligible(PLAYER, restarted.kit("once").unwrap(), u64::MAX),
            Err(Error::Once)
        );
    }

    #[test]
    fn double_click_and_ambiguous_restart_never_create_another_operation() {
        let mut state = Catalog::default();
        state.create("daily", vec![item(0)], 1, false).unwrap();
        let first = state.begin(PLAYER, "daily", 100).unwrap();
        assert_eq!(state.begin(PLAYER, "daily", 100), Err(Error::Pending));
        let mut restarted = state.clone();
        assert_eq!(
            restarted.begin(PLAYER, "daily", u64::MAX),
            Err(Error::Pending)
        );
        assert_eq!(
            restarted.complete(PLAYER, "daily", first + 1),
            Err(Error::Invalid)
        );
        restarted.complete(PLAYER, "daily", first).unwrap();
        assert_eq!(restarted.complete(PLAYER, "daily", first), Ok(()));
    }

    #[test]
    fn full_inventory_plan_is_all_or_nothing() {
        let mut state = Catalog::default();
        state
            .create("daily", vec![item(0), item(1)], 10, false)
            .unwrap();
        let mut occupied = [true; SLOT_COUNT];
        occupied[2] = false;
        let kit = state.kit("daily").unwrap();
        assert_eq!(
            plan(kit, &unstackable(kit), &self::occupied(&occupied)),
            Err(Error::Full)
        );
        assert!(state.claims.is_empty());
    }

    #[test]
    fn armor_and_offhand_use_empty_equipment_slots_and_never_overwrite() {
        let mut state = Catalog::default();
        let entries = vec![item(0), item(36), item(37), item(38), item(39), item(40)];
        state.create("gear", entries.clone(), 10, false).unwrap();
        let kit = state.kit("gear").unwrap();
        let traits = unstackable(kit);
        assert_eq!(
            plan(kit, &traits, &empty())
                .unwrap()
                .iter()
                .map(|piece| piece.slot)
                .collect::<Vec<_>>(),
            entries.iter().map(|entry| entry.slot).collect::<Vec<_>>()
        );
        let mut slots = [false; SLOT_COUNT];
        slots[39] = true;
        let planned = plan(kit, &traits, &occupied(&slots)).unwrap();
        assert!(!planned.iter().any(|piece| piece.slot == 39));
        assert_eq!(planned.len(), 6);
        assert!(planned.iter().all(|piece| !piece.merge && piece.count == 1));
    }

    #[test]
    fn invalid_names_empty_oversized_and_duplicate_slots_are_rejected() {
        let mut state = Catalog::default();
        for name in ["", "../kit", "create", "Daily", "é", "a.b", "a/b"] {
            assert_eq!(
                state.create(name, vec![item(0)], 1, false),
                Err(Error::Invalid)
            );
        }
        assert_eq!(state.create("empty", vec![], 1, false), Err(Error::Empty));
        assert_eq!(
            state.create("zero", vec![item(0)], 0, false),
            Err(Error::Invalid)
        );
        assert_eq!(
            state.create("large", vec![item(0)], MAX_COOLDOWN + 1, false),
            Err(Error::Invalid)
        );
        assert_eq!(
            state.create("dupe", vec![item(0), item(0)], 1, false),
            Err(Error::Invalid)
        );
        let mut huge = item(1);
        huge.stack.data = vec![0; MAX_ITEM_BYTES + 1];
        assert_eq!(
            state.create("huge", vec![huge], 1, false),
            Err(Error::Limit)
        );
        assert_eq!(state.next_id, 3);
    }

    #[test]
    fn strict_uuid_parsing_accepts_canonical_ids_only() {
        assert_eq!(
            player_id("07070707-0707-0707-0707-070707070707"),
            Ok(PLAYER)
        );
        for uuid in [
            "KitA",
            "",
            "../",
            "07070707070707070707070707070707",
            "07070707-0707-0707-0707-07070707070z",
        ] {
            assert_eq!(player_id(uuid), Err(Error::Invalid));
        }
    }

    #[test]
    fn bounds_fail_closed_without_evicting_one_time_history() {
        let mut state = Catalog::default();
        state.create("once", vec![item(0)], 1, true).unwrap();
        for index in 0..MAX_CLAIMS {
            let mut player = [0; 16];
            player[..8].copy_from_slice(&(index as u64).to_le_bytes());
            state.claims.insert(
                (player, 3),
                Claim {
                    at: 100,
                    operation: 1,
                    pending: false,
                },
            );
        }
        assert_eq!(state.begin([255; 16], "once", 200), Err(Error::Limit));
        assert_eq!(state.claims.len(), MAX_CLAIMS);
    }

    #[test]
    fn default_kits_and_cooldowns_remain_compatible() {
        let state = Catalog::default();
        assert_eq!(state.kit("starter").unwrap().cooldown, 600);
        assert_eq!(state.kit("tools").unwrap().cooldown, 3600);
        assert_eq!(state.kit("starter").unwrap().entries.len(), 4);
        assert_eq!(state.validate(), Ok(()));
    }
}
