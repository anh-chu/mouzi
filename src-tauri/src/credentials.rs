use crate::db;

// Older development builds wrote this value into SQLite. Move it only after the
// OS credential store has accepted it, so an unavailable store cannot lose it.
pub fn migrate_legacy() -> Result<(), String> {
    let legacy: String = {
        let db = db::get_db();
        let conn = db.lock().map_err(|_| "Settings are unavailable")?;
        conn.query_row("SELECT ai_api_key FROM settings LIMIT 1", [], |r| r.get(0))
            .map_err(|_| "Could not read legacy API key")?
    };
    if !legacy.is_empty() {
        if load()?.is_none() { save(&legacy)?; }
        clear_legacy()?;
    }
    Ok(())
}

pub fn clear_legacy() -> Result<(), String> {
    let db = db::get_db();
    let conn = db.lock().map_err(|_| "Settings are unavailable")?;
    conn.execute_batch("PRAGMA secure_delete=ON; UPDATE settings SET ai_api_key='' WHERE ai_api_key<>'';")
        .map_err(|_| "Could not clear legacy API key".to_string())
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{ffi::c_void, ptr, slice};
    #[repr(C)]
    struct Credential {
        flags: u32, kind: u32, target: *mut u16, comment: *mut u16,
        last_written: i64, blob_size: u32, blob: *mut u8, persist: u32,
        attribute_count: u32, attributes: *mut c_void, alias: *mut u16, username: *mut u16,
    }
    #[link(name = "Advapi32")]
    extern "system" {
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredReadW(target: *const u16, kind: u32, flags: u32, credential: *mut *mut Credential) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }
    #[link(name = "Kernel32")]
    extern "system" { fn GetLastError() -> u32; }
    fn target() -> Vec<u16> { "Mouzi/OpenRouter/Jev".encode_utf16().chain(Some(0)).collect() }
    pub fn save(key: &str) -> Result<(), String> {
        if key.len() > 2560 { return Err("API key is too long".into()); }
        let mut target = target();
        let credential = Credential { flags: 0, kind: 1, target: target.as_mut_ptr(), comment: ptr::null_mut(),
            last_written: 0, blob_size: key.len() as u32, blob: key.as_ptr() as *mut u8,
            persist: 2, attribute_count: 0, attributes: ptr::null_mut(), alias: ptr::null_mut(), username: ptr::null_mut() };
        if unsafe { CredWriteW(&credential, 0) } == 0 { return Err("Windows Credential Manager could not save the API key".into()); }
        Ok(())
    }
    pub fn load() -> Result<Option<String>, String> {
        let mut credential: *mut Credential = ptr::null_mut();
        if unsafe { CredReadW(target().as_ptr(), 1, 0, &mut credential) } == 0 {
            return if unsafe { GetLastError() } == 1168 { Ok(None) } else { Err("Windows Credential Manager could not read the API key".into()) };
        }
        let result = unsafe {
            let bytes = slice::from_raw_parts((*credential).blob, (*credential).blob_size as usize);
            let result = String::from_utf8(bytes.to_vec()).map_err(|_| "Stored API key is invalid".to_string());
            CredFree(credential.cast());
            result
        }?;
        Ok(Some(result))
    }
    pub fn remove() -> Result<(), String> {
        if unsafe { CredDeleteW(target().as_ptr(), 1, 0) } == 0 && unsafe { GetLastError() } != 1168 {
            return Err("Windows Credential Manager could not remove the API key".into());
        }
        Ok(())
    }
}
#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn save(_: &str) -> Result<(), String> { Err("Secure API key storage is currently available on Windows only".into()) }
    pub fn load() -> Result<Option<String>, String> { Err("Secure API key storage is currently available on Windows only".into()) }
    pub fn remove() -> Result<(), String> { Err("Secure API key storage is currently available on Windows only".into()) }
}
pub use platform::{load, remove, save};
