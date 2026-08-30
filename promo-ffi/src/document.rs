//! Document FFI (Stage 2, slice 1): the core owns the document; a front
//! end holds a handle, sends commands as JSON, and re-reads. Undo lives
//! here because there is only one place edits happen.

use promo_editor::{Command, Document};
use std::ffi::{c_char, c_int, CStr, CString};

/// Opaque to C.
pub struct DocHandle {
    document: Document,
}

/// Parses a metadata.json payload into an owned document. NULL on a
/// payload the model refuses (reason on stderr). Free with
/// `promo_doc_free`.
///
/// Safety contract (C ABI): `json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_open(json: *const c_char) -> *mut DocHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if json.is_null() {
            return std::ptr::null_mut();
        }
        let Ok(text) = (unsafe { CStr::from_ptr(json) }).to_str() else {
            return std::ptr::null_mut();
        };
        match Document::open(text) {
            Ok(document) => Box::into_raw(Box::new(DocHandle { document })),
            Err(e) => {
                eprintln!("promo_doc_open: {e}");
                std::ptr::null_mut()
            }
        }
    })
}

/// Safety contract (C ABI): `handle` came from this library, freed at
/// most once. Null is a no-op.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_free(handle: *mut DocHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
}

/// The document as canonical JSON. Free with `promo_string_free`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_to_json(handle: *const DocHandle) -> *mut c_char {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return std::ptr::null_mut();
        };
        handle
            .document
            .to_json()
            .ok()
            .and_then(|s| CString::new(s).ok())
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut())
    })
}

/// Bumps on every applied command, undo and redo — what a front end
/// watches to know its projections are stale. u64::MAX on a bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_version(handle: *const DocHandle) -> u64 {
    crate::ffi_guard(u64::MAX, move || match unsafe { handle.as_ref() } {
        Some(handle) => handle.document.version(),
        None => u64::MAX,
    })
}

/// Applies one command (`{"kind": "renameLayer", "layerID": …, …}`).
/// 0 ok, -1 bad handle/pointer, -2 the JSON is not a command, -3 the
/// command failed (unknown layer, bad value — reason on stderr; the
/// document, version and history are untouched).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_apply(handle: *mut DocHandle, command_json: *const c_char) -> c_int {
    crate::ffi_guard(-3, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if command_json.is_null() {
            return -1;
        }
        let Ok(text) = (unsafe { CStr::from_ptr(command_json) }).to_str() else {
            return -1;
        };
        let command: Command = match serde_json::from_str(text) {
            Ok(command) => command,
            Err(e) => {
                eprintln!("promo_doc_apply: {e}");
                return -2;
            }
        };
        match handle.document.apply(&command) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("promo_doc_apply: {e}");
                -3
            }
        }
    })
}

/// 1 = a step was undone/redone, 0 = nothing to step. -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_undo(handle: *mut DocHandle) -> c_int {
    crate::ffi_guard(-1, move || match unsafe { handle.as_mut() } {
        Some(handle) => handle.document.undo() as c_int,
        None => -1,
    })
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_redo(handle: *mut DocHandle) -> c_int {
    crate::ffi_guard(-1, move || match unsafe { handle.as_mut() } {
        Some(handle) => handle.document.redo() as c_int,
        None => -1,
    })
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_can_undo(handle: *const DocHandle) -> c_int {
    crate::ffi_guard(0, move || match unsafe { handle.as_ref() } {
        Some(handle) => handle.document.can_undo() as c_int,
        None => 0,
    })
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_doc_can_redo(handle: *const DocHandle) -> c_int {
    crate::ffi_guard(0, move || match unsafe { handle.as_ref() } {
        Some(handle) => handle.document.can_redo() as c_int,
        None => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_round_trip_applies_and_undoes() {
        let spec: promo_editor::author::AuthorSpec = serde_json::from_value(serde_json::json!({
            "name": "Doc", "createdAt": 1000.0,
            "slides": [{"filename": "a.png", "kind": "image",
                        "pixelWidth": 10.0, "pixelHeight": 10.0}],
        }))
        .unwrap();
        let json = CString::new(promo_editor::author::author(&spec).unwrap()).unwrap();
        let handle = promo_doc_open(json.as_ptr());
        assert!(!handle.is_null());
        assert_eq!(promo_doc_version(handle), 0);

        // The background layer's id, read back out of the document itself.
        let out = promo_doc_to_json(handle);
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        crate::project::promo_string_free(out);
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let id = doc["layers"][0]["id"].as_str().unwrap();

        let cmd = CString::new(format!(
            r#"{{"kind": "renameLayer", "layerID": "{id}", "name": "Ground"}}"#
        ))
        .unwrap();
        assert_eq!(promo_doc_apply(handle, cmd.as_ptr()), 0);
        assert_eq!(promo_doc_version(handle), 1);
        assert_eq!(promo_doc_can_undo(handle), 1);

        let bad =
            CString::new(r#"{"kind": "renameLayer", "layerID": "GHOST", "name": "x"}"#).unwrap();
        assert_eq!(promo_doc_apply(handle, bad.as_ptr()), -3);
        assert_eq!(
            promo_doc_version(handle),
            1,
            "a failed command bumps nothing"
        );

        assert_eq!(promo_doc_undo(handle), 1);
        assert_eq!(promo_doc_undo(handle), 0);
        let restored = promo_doc_to_json(handle);
        let restored_text = unsafe { CStr::from_ptr(restored) }
            .to_str()
            .unwrap()
            .to_string();
        crate::project::promo_string_free(restored);
        assert_eq!(restored_text, text);
        promo_doc_free(handle);
    }
}
