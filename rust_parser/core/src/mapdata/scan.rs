//! SaveScan -- port of sav_map_data.SaveScan (lines ~782-862): the single
//! O(n) pass over the parsed save shared by every collector and the save
//! index. Insertion order is load-bearing everywhere (Python dict semantics:
//! last value wins, first position kept).

use super::consts::GAME_STATE_TYPE_PATH_SUBSTRING;
use crate::extract::InstanceSlots;
use crate::store::*;
use indexmap::IndexMap;
use std::cell::OnceCell;
use std::collections::HashMap;

/// (levelIdx, slotIdx) into store.levels[li].headers / .objects (the two are
/// index-aligned).
pub type Slot = (usize, usize);

pub struct SaveScan<'a> {
    pub store: &'a SaveStore,
    /// instanceName -> slot of the header (last value wins, first position
    /// kept -- one map serves both headersByInstanceName and
    /// objectsByInstanceName since headers/objects are index-aligned).
    pub by_instance_name: IndexMap<&'a [u8], Slot>,
    /// typePath -> [(globalSeq, slot)] for actor headers, in first-encounter
    /// key order and per-bucket save order.
    pub actor_seqs_by_type_path: IndexMap<&'a [u8], Vec<(usize, Slot)>>,
    /// BP_GameState_C objects (matched on instanceName substring) in save
    /// order.
    pub game_state_objects: Vec<Slot>,
    /// Cached extract::InstanceSlots view of by_instance_name -- built on
    /// first use and shared by every bulk-extractor call (previously each
    /// call re-walked all objects: 6+ redundant full passes per load).
    instance_slots: OnceCell<InstanceSlots<'a>>,
}

impl<'a> SaveScan<'a> {
    pub fn new(store: &'a SaveStore) -> Self {
        let data: &[u8] = &store.data;
        let mut by_instance_name: IndexMap<&[u8], Slot> = IndexMap::new();
        let mut actor_seqs_by_type_path: IndexMap<&[u8], Vec<(usize, Slot)>> = IndexMap::new();
        let mut game_state_objects: Vec<Slot> = Vec::new();
        let mut seq = 0usize;
        for (li, level) in store.levels.iter().enumerate() {
            for (oi, header) in level.headers.iter().enumerate() {
                let name = header.instance_name().bytes(data);
                // Python dict: last value wins, first position kept.
                by_instance_name.insert(name, (li, oi));
                if let Header::Actor(actor) = header {
                    actor_seqs_by_type_path
                        .entry(actor.type_path.bytes(data))
                        .or_default()
                        .push((seq, (li, oi)));
                }
                seq += 1;
                if find_subslice(name, GAME_STATE_TYPE_PATH_SUBSTRING.as_bytes()) {
                    game_state_objects.push((li, oi));
                }
            }
        }
        SaveScan {
            store,
            by_instance_name,
            actor_seqs_by_type_path,
            game_state_objects,
            instance_slots: OnceCell::new(),
        }
    }

    /// The extractor-shaped (slots, slotIndexByName) pair, derived from
    /// by_instance_name (identical content and order: last value wins, first
    /// position kept).
    pub fn instance_slots(&self) -> &InstanceSlots<'a> {
        self.instance_slots.get_or_init(|| {
            let slots: Vec<(&[u8], Slot)> =
                self.by_instance_name.iter().map(|(&name, &slot)| (name, slot)).collect();
            let slot_by_name: HashMap<&[u8], usize> =
                slots.iter().enumerate().map(|(i, &(name, _))| (name, i)).collect();
            (slots, slot_by_name)
        })
    }

    #[inline]
    pub fn data(&self) -> &'a [u8] {
        &self.store.data
    }

    #[inline]
    pub fn header(&self, slot: Slot) -> &'a Header {
        &self.store.levels[slot.0].headers[slot.1]
    }

    #[inline]
    pub fn actor(&self, slot: Slot) -> &'a ActorHeader {
        match self.header(slot) {
            Header::Actor(a) => a,
            Header::Component(_) => panic!("actor slot points at a component header"),
        }
    }

    #[inline]
    pub fn object(&self, slot: Slot) -> &'a Object {
        &self.store.levels[slot.0].parsed_objects()[slot.1]
    }

    /// scan.objectsByInstanceName.get(name).
    pub fn object_by_name(&self, name: &[u8]) -> Option<&'a Object> {
        self.by_instance_name.get(name).map(|&slot| self.object(slot))
    }

    pub fn header_by_name(&self, name: &[u8]) -> Option<&'a Header> {
        self.by_instance_name.get(name).map(|&slot| self.header(slot))
    }

    /// scan.actorHeadersOfType(*typePaths): actor slots in global save order.
    pub fn actor_slots_of_type(&self, type_paths: &[&str]) -> Vec<Slot> {
        if type_paths.len() == 1 {
            return match self.actor_seqs_by_type_path.get(type_paths[0].as_bytes()) {
                Some(entries) => entries.iter().map(|&(_, slot)| slot).collect(),
                None => Vec::new(),
            };
        }
        let mut merged: Vec<(usize, Slot)> = Vec::new();
        for type_path in type_paths {
            if let Some(entries) = self.actor_seqs_by_type_path.get(type_path.as_bytes()) {
                merged.extend_from_slice(entries);
            }
        }
        merged.sort_by_key(|&(seq, _)| seq);
        merged.into_iter().map(|(_, slot)| slot).collect()
    }

    /// scan.actorsOfType(*typePaths): (actorSlot, objectSlotOrNone) pairs --
    /// the object matched by instanceName, exactly like Python (an actor
    /// whose name was shadowed by a later duplicate resolves to the LAST
    /// same-named object).
    pub fn actors_of_type(&self, type_paths: &[&str]) -> Vec<(Slot, Option<Slot>)> {
        let data = self.data();
        self.actor_slots_of_type(type_paths)
            .into_iter()
            .map(|slot| {
                let name = self.header(slot).instance_name().bytes(data);
                (slot, self.by_instance_name.get(name).copied())
            })
            .collect()
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
