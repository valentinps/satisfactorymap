//! SaveScan -- port of sav_map_data.SaveScan (lines ~782-862): the single
//! O(n) pass over the parsed save shared by every collector and the save
//! index. Insertion order is load-bearing everywhere (Python dict semantics:
//! last value wins, first position kept).

use super::consts::{GAME_STATE_TYPE_PATH_SUBSTRING, LIGHTWEIGHT_BUILDABLE_SUBSYSTEM_TYPE_PATH};
use crate::extract::InstanceSlots;
use crate::store::*;
use indexmap::IndexMap;
use std::cell::{OnceCell, RefCell};
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
    /// Memoized FGLightweightBuildableSubsystem object. Its actor_specific
    /// holds every lightweight instance (~100k transforms on the big save)
    /// and three consumers read it, so parse it once and hand out borrows --
    /// never clone.
    lightweight_subsystem: OnceCell<Option<Object>>,
    /// Memoized collector outputs consumed by BOTH the payload build and
    /// the save-index build (see build_all_json): collectables, hard drives,
    /// depot contents, and the uncollected-catalog-drop list. Without these
    /// each ran twice per full load -- and collect_collectables alone walks
    /// every level's (huge) collectables lists five times per call.
    collectables: OnceCell<serde_json::Value>,
    hard_drives: OnceCell<serde_json::Value>,
    depot_contents: OnceCell<serde_json::Value>,
    catalog_drops: OnceCell<Vec<(&'static str, i64, [f64; 3], &'static str)>>,
    /// First on-demand re-parse failure seen during a build. Re-parsing bytes
    /// that already parsed cannot fail, but once the pipeline is lean the
    /// build IS the deep validation of an edited body -- so build_all_json
    /// turns a latched error into Err instead of emitting a wrong payload.
    parse_error: RefCell<Option<String>>,
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
            lightweight_subsystem: OnceCell::new(),
            collectables: OnceCell::new(),
            hard_drives: OnceCell::new(),
            depot_contents: OnceCell::new(),
            catalog_drops: OnceCell::new(),
            parse_error: RefCell::new(None),
        }
    }

    /// collectors::world::collect_collectables, computed once per scan.
    pub fn collectables(&self) -> &serde_json::Value {
        self.collectables.get_or_init(|| super::collectors::world::collect_collectables(self))
    }

    /// collectors::world::collect_hard_drives, computed once per scan.
    pub fn hard_drives(&self) -> &serde_json::Value {
        self.hard_drives.get_or_init(|| super::collectors::world::collect_hard_drives(self))
    }

    /// collectors::simple::collect_dimensional_depot_contents, once per scan.
    pub fn depot_contents(&self) -> &serde_json::Value {
        self.depot_contents
            .get_or_init(|| super::collectors::simple::collect_dimensional_depot_contents(self))
    }

    /// collectors::world::uncollected_catalog_drops, computed once per scan.
    pub fn uncollected_catalog_drops(&self) -> &[(&'static str, i64, [f64; 3], &'static str)] {
        self.catalog_drops
            .get_or_init(|| super::collectors::world::uncollected_catalog_drops(self))
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

    /// Re-parse the object at `slot` from its byte span (owned; identical to
    /// the eager parse). Returns None on failure, latching the first error so
    /// the build can fail cleanly. Replaces the old `object()` borrow into the
    /// resident model, so the builder never needs `parsed_objects()`.
    pub fn parse_object(&self, slot: Slot) -> Option<Object> {
        match self.store.parse_object_at(slot.0, slot.1) {
            Ok(object) => Some(object),
            Err(e) => {
                let mut latched = self.parse_error.borrow_mut();
                if latched.is_none() {
                    *latched = Some(format!("object at {slot:?}: {e}"));
                }
                None
            }
        }
    }

    /// scan.objectsByInstanceName.get(name), re-parsed on demand.
    pub fn parse_object_by_name(&self, name: &[u8]) -> Option<Object> {
        let slot = *self.by_instance_name.get(name)?;
        self.parse_object(slot)
    }

    /// The FGLightweightBuildableSubsystem object (first of its type, resolved
    /// by instance name -- last value wins, exactly like the old lookup),
    /// parsed once and memoized. None when the save has no such subsystem.
    pub fn lightweight_subsystem_object(&self) -> Option<&Object> {
        self.lightweight_subsystem
            .get_or_init(|| {
                let slots =
                    self.actor_slots_of_type(&[LIGHTWEIGHT_BUILDABLE_SUBSYSTEM_TYPE_PATH]);
                let &first = slots.first()?;
                let name = self.actor(first).instance_name.bytes(self.data());
                self.parse_object_by_name(name)
            })
            .as_ref()
    }

    /// The first on-demand re-parse error latched during the build, if any.
    pub fn parse_error(&self) -> Option<String> {
        self.parse_error.borrow().clone()
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
