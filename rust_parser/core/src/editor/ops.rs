//! Edit operations: the JSON wire format between the frontend and the edit
//! engine. Each op references the body state produced by all prior ops
//! (names of pasted objects, lightweight indices), so replaying the list in
//! order from the pristine body is deterministic -- that's what makes undo
//! (drop the last op, replay) correct.

use crate::error::{perr, PResult};
use serde::{Deserialize, Serialize};

/// Parse a JSON array of ops (the worker RPC payload). Lives here so the
/// wasm crate doesn't need its own serde_json dependency.
pub fn parse_ops_json(json: &str) -> PResult<Vec<EditOp>> {
    serde_json::from_str(json).map_err(|e| perr!("Bad edit ops: {}", e))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum EditOp {
    #[serde(rename_all = "camelCase")]
    MoveActors {
        /// Full instance names (persistent-level path names).
        names: Vec<String>,
        /// World-space delta in centimeters.
        delta: [f64; 3],
        /// Additional world yaw in degrees (90-degree steps from the UI).
        #[serde(default)]
        rotate_yaw_deg: f64,
        /// World XY to rotate around; required when rotate_yaw_deg != 0.
        #[serde(default)]
        pivot: Option<[f64; 2]>,
    },
    #[serde(rename_all = "camelCase")]
    MoveLightweight {
        items: Vec<LwRef>,
        delta: [f64; 3],
        #[serde(default)]
        rotate_yaw_deg: f64,
        #[serde(default)]
        pivot: Option<[f64; 2]>,
    },
    #[serde(rename_all = "camelCase")]
    DuplicateActors {
        names: Vec<String>,
        delta: [f64; 3],
        #[serde(default)]
        rotate_yaw_deg: f64,
        #[serde(default)]
        pivot: Option<[f64; 2]>,
        /// Makes generated instance names deterministic across replays.
        seed: u64,
    },
    #[serde(rename_all = "camelCase")]
    DuplicateLightweight {
        items: Vec<LwRef>,
        delta: [f64; 3],
        #[serde(default)]
        rotate_yaw_deg: f64,
        #[serde(default)]
        pivot: Option<[f64; 2]>,
    },
    #[serde(rename_all = "camelCase")]
    DeleteActors { names: Vec<String> },
    #[serde(rename_all = "camelCase")]
    DeleteLightweight { items: Vec<LwRef> },
}

/// A lightweight buildable has no instance name; it's addressed by its
/// subsystem group type path + index within the group (the same index the
/// synthetic "LightweightBuildable:<typePath>:<idx>" map ids use).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LwRef {
    pub type_path: String,
    pub index: u32,
}
