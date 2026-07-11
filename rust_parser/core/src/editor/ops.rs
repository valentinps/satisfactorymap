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
    /// Paste objects copied from ANOTHER session/save (the cross-tab
    /// clipboard): raw header/body byte blobs plus the version metadata
    /// needed to validate they're byte-compatible with this save. Produced
    /// by editor::clipboard::extract_clipboard on the source side.
    #[serde(rename_all = "camelCase")]
    PasteExternal {
        save_version: u32,
        object_version: i32,
        #[serde(default)]
        lightweight_version: Option<u32>,
        #[serde(default)]
        actors: Vec<ForeignActor>,
        #[serde(default)]
        lightweight: Vec<ForeignLightweight>,
        /// World XY the blobs were copied around; rotation pivots here and
        /// `delta` translates from here to the paste point.
        anchor: [f64; 2],
        delta: [f64; 3],
        #[serde(default)]
        rotate_yaw_deg: f64,
        seed: u64,
    },
}

/// One copied actor/component: base64 of its header record and object
/// record, verbatim from the source save's decompressed body.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignActor {
    pub h: String,
    pub b: String,
}

/// One copied lightweight buildable record (blueprint proxy already emptied
/// at extraction, so the record layout is fixed).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignLightweight {
    pub type_path: String,
    pub r: String,
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
