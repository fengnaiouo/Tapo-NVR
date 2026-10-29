//! 機密資料加密（攝影機密碼、TP-Link 雲端密碼、ntfy 權杖）
//!
//! - Windows：DPAPI（CryptProtectData），金鑰綁定目前的 Windows 使用者帳號；
//!   資料庫被複製到其他帳號或電腦都無法解密
//! - 其他系統：資料夾內的隨機金鑰檔（權限 600）+ AES-128-CCM

use std::path::Path;

use anyhow::{anyhow, Result};

/// 額外熵值：其他程式就算以同一個 Windows 帳號呼叫 DPAPI，也必須知道這個值
const ENTROPY: &[u8] = b"tapo-nvr/secret/v1";

#[cfg(windows)]
pub fn protect(_dir: &Path, plain: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: plain.len() as u32, pbData: plain.as_ptr() as *mut u8 };
        let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err(anyhow!("CryptProtectData failed: {}", std::io::Error::last_os_error()));
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Ok(v)
    }
}

#[cfg(windows)]
pub fn unprotect(_dir: &Path, data: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 {
            return Err(anyhow!(
                "CryptUnprotectData failed（資料庫可能來自其他 Windows 帳號或電腦）: {}",
                std::io::Error::last_os_error()
            ));
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Ok(v)
    }
}

#[cfg(not(windows))]
fn key(dir: &Path) -> Result<[u8; 16]> {
    use std::io::Write;
    let path = dir.join("secret.key");
    if let Ok(b) = std::fs::read(&path) {
        if b.len() == 16 {
            let mut k = [0u8; 16];
            k.copy_from_slice(&b);
            return Ok(k);
        }
    }
    let mut k = [0u8; 16];
    getrandom::getrandom(&mut k).map_err(|e| anyhow!("{e}"))?;
    let mut f = std::fs::File::create(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(&k)?;
    Ok(k)
}

#[cfg(not(windows))]
pub fn protect(dir: &Path, plain: &[u8]) -> Result<Vec<u8>> {
    use ccm::aead::{Aead, KeyInit, Payload};
    type C = ccm::Ccm<aes::Aes128, ccm::consts::U16, ccm::consts::U12>;
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| anyhow!("{e}"))?;
    let ct = C::new(&key(dir)?.into())
        .encrypt(ccm::Nonce::<ccm::consts::U12>::from_slice(&nonce), Payload { msg: plain, aad: ENTROPY })
        .map_err(|_| anyhow!("encrypt"))?;
    Ok([nonce.to_vec(), ct].concat())
}

#[cfg(not(windows))]
pub fn unprotect(dir: &Path, data: &[u8]) -> Result<Vec<u8>> {
    use ccm::aead::{Aead, KeyInit, Payload};
    type C = ccm::Ccm<aes::Aes128, ccm::consts::U16, ccm::consts::U12>;
    if data.len() < 12 {
        return Err(anyhow!("bad secret"));
    }
    C::new(&key(dir)?.into())
        .decrypt(ccm::Nonce::<ccm::consts::U12>::from_slice(&data[..12]), Payload { msg: &data[12..], aad: ENTROPY })
        .map_err(|_| anyhow!("decrypt"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir();
        let enc = super::protect(&dir, b"p@ss word").unwrap();
        assert_ne!(enc, b"p@ss word");
        assert_eq!(super::unprotect(&dir, &enc).unwrap(), b"p@ss word");
    }
}
