//! Inventory capture and delivery using the public item and inventory API.
use crate::{
    component_kinds::KINDS,
    item_data,
    model::{
        Components, Entry, Error, Given, Held, ItemTraits, Kit, Piece, SLOT_COUNT, Slots, Stack,
        deliver, plan,
    },
};
use strawberry_plugin_api::{
    ItemStack, Player,
    inventory::{Inventory, PlayerInventory},
};

pub fn capture(player: &Player) -> Result<Vec<Entry>, Error> {
    let inventory = player.get_inventory();
    let main = inventory.as_inventory();
    if main.get_size() < 36 {
        return Err(Error::Invalid);
    }
    let mut entries = Vec::new();
    for slot in 0..36 {
        if let Some(stack) = main.get_item(slot).filter(nonempty) {
            entries.push(Entry {
                slot: slot as u8,
                stack: snapshot(&stack)?,
            });
        }
    }
    let equipment = [
        inventory.get_boots(),
        inventory.get_leggings(),
        inventory.get_chestplate(),
        inventory.get_helmet(),
        inventory.get_off_hand(),
    ];
    for (index, item) in equipment.into_iter().enumerate() {
        if let Some(stack) = item.filter(nonempty) {
            entries.push(Entry {
                slot: 36 + index as u8,
                stack: snapshot(&stack)?,
            });
        }
    }
    if entries.is_empty() {
        return Err(Error::Empty);
    }
    Ok(entries)
}
fn nonempty(stack: &ItemStack) -> bool {
    stack.get_count() != 0 && !matches!(stack.get_registry_key().as_str(), "minecraft:air" | "air")
}
fn snapshot(stack: &ItemStack) -> Result<Stack, Error> {
    let mut components: Vec<_> = stack
        .get_components()
        .into_iter()
        .map(|value| (value.component as u16, value.value))
        .collect();
    components.sort_by_key(|(kind, _)| *kind);
    let count = stack.get_count();
    if count == 0 || count > stack.get_max_count() {
        return Err(Error::Invalid);
    }
    Ok(Stack {
        key: {
            let key = stack.get_registry_key();
            if key.contains(':') {
                key
            } else {
                format!("minecraft:{key}")
            }
        },
        count,
        data: item_data::encode(&components)?,
    })
}
/// A full stack for display. A defined line above the item's maximum shows one legal stack.
pub fn render(stack: &Stack) -> Result<ItemStack, Error> {
    let max = traits(stack)?.max;
    build(stack, stack.count.min(max))
}
fn build(stack: &Stack, count: u8) -> Result<ItemStack, Error> {
    let item = ItemStack::new(&stack.key, count);
    if !stack.data.is_empty() {
        let expected = item_data::decode(&stack.data)?;
        for (kind, value) in &expected {
            let component = *KINDS.get(usize::from(*kind)).ok_or(Error::Invalid)?;
            item.set_component(component, value);
        }
        let actual: std::collections::BTreeMap<_, _> = item
            .get_components()
            .into_iter()
            .map(|value| (value.component as u16, value.value))
            .collect();
        if expected
            .iter()
            .any(|(kind, value)| actual.get(kind) != Some(value))
        {
            return Err(Error::Invalid);
        }
    }
    if !same_key(&item.get_registry_key(), &stack.key)
        || item.get_count() != count
        || count > item.get_max_count()
    {
        return Err(Error::Invalid);
    }
    Ok(item)
}
fn same_key(left: &str, right: &str) -> bool {
    left.trim_start_matches("minecraft:") == right.trim_start_matches("minecraft:")
}
/// An unknown key becomes air on the server, so it is checked by name.
pub fn known_item(key: &str) -> bool {
    same_key(&ItemStack::new(key, 1).get_registry_key(), key)
}
fn traits(stack: &Stack) -> Result<ItemTraits, Error> {
    let item = build(stack, 1)?;
    Ok(ItemTraits {
        max: item.get_max_count(),
        components: components(&item),
    })
}
fn components(stack: &ItemStack) -> Components {
    let mut out: Components = stack
        .get_components()
        .into_iter()
        .map(|value| (value.component as u16, value.value))
        .collect();
    out.sort_by_key(|(kind, _)| *kind);
    out
}
fn full_key(stack: &ItemStack) -> String {
    let key = stack.get_registry_key();
    if key.contains(':') {
        key
    } else {
        format!("minecraft:{key}")
    }
}
fn held(item: Option<ItemStack>) -> Held {
    match item.filter(nonempty) {
        Some(stack) => Held::Item {
            key: full_key(&stack),
            count: stack.get_count(),
            max: stack.get_max_count(),
            components: components(&stack),
        },
        None => Held::Empty,
    }
}

struct Live<'a> {
    player: &'a PlayerInventory,
    main: &'a Inventory,
    kit: &'a Kit,
}
impl Slots for Live<'_> {
    fn read(&self, slot: u8) -> Held {
        held(match slot {
            0..36 => self.main.get_item(u32::from(slot)),
            36 => self.player.get_boots(),
            37 => self.player.get_leggings(),
            38 => self.player.get_chestplate(),
            39 => self.player.get_helmet(),
            40 => self.player.get_off_hand(),
            _ => None,
        })
    }
    fn write(&mut self, piece: &Piece) -> bool {
        let slot = u32::from(piece.slot);
        if piece.merge {
            let Some(stack) = self.main.get_item(slot) else {
                return false;
            };
            stack.set_count(stack.get_count().saturating_add(piece.count));
            self.main.set_item(slot, Some(stack));
            return true;
        }
        let Some(entry) = self.kit.entries.get(piece.entry) else {
            return false;
        };
        let Ok(stack) = build(&entry.stack, piece.count) else {
            return false;
        };
        match piece.slot {
            0..36 => self.main.set_item(slot, Some(stack)),
            36 => self.player.set_boots(Some(stack)),
            37 => self.player.set_leggings(Some(stack)),
            38 => self.player.set_chestplate(Some(stack)),
            39 => self.player.set_helmet(Some(stack)),
            40 => self.player.set_off_hand(Some(stack)),
            _ => return false,
        }
        true
    }
}

pub struct Prepared {
    player: PlayerInventory,
    main: Inventory,
    traits: Vec<ItemTraits>,
    from: [Held; SLOT_COUNT],
    pieces: Vec<Piece>,
}
impl Prepared {
    /// Called only after the use record was successfully synced and replaced.
    pub fn give(self, kit: &Kit) -> Given {
        let mut live = Live {
            player: &self.player,
            main: &self.main,
            kit,
        };
        deliver(kit, &self.traits, &self.from, self.pieces, &mut live)
    }
}
/// Everything that can refuse a claim happens here, before a use is recorded.
pub fn prepare(player: &Player, kit: &Kit) -> Result<Prepared, Error> {
    let inventory = player.get_inventory();
    let main = inventory.as_inventory();
    if main.get_size() < 36 {
        return Err(Error::Invalid);
    }
    let traits = kit
        .entries
        .iter()
        .map(|entry| traits(&entry.stack))
        .collect::<Result<Vec<_>, _>>()?;
    let live = Live {
        player: &inventory,
        main: &main,
        kit,
    };
    let from: [Held; SLOT_COUNT] = std::array::from_fn(|slot| live.read(slot as u8));
    let pieces = plan(kit, &traits, &from)?;
    Ok(Prepared {
        player: inventory,
        main,
        traits,
        from,
        pieces,
    })
}
