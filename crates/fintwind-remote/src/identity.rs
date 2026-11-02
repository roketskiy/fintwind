use std::{
    io::{Read as _, Write as _},
    path::Path,
};

use anyhow::{Context as _, bail};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{self, KeyPair as _},
};
use uuid::Uuid;

use crate::crypto;

const FILE_NAME: &str = "remote-control-identity.bin";
const MAX_FILE_BYTES: u64 = 4096;

/// Installation signing key. Never formatted or logged.
pub(crate) struct Identity {
    key: signature::Ed25519KeyPair,
    public_key: [u8; 32],
    installation_id: String,
}

impl Identity {
    pub(crate) fn load_or_create() -> anyhow::Result<Self> {
        let path = crate::dirs_path()?.join(".fintwind").join(FILE_NAME);
        if let Some(stored) = read_stored(&path)? {
            return Self::from_stored(&stored);
        }
        let pkcs8 = signature::Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("could not generate the installation identity"))?;
        publish(&path, pkcs8.as_ref())?;
        // Another start may have won the create race. The file on disk decides.
        let stored = read_stored(&path)?.context("installation identity is missing")?;
        Self::from_stored(&stored)
    }

    pub(crate) fn public_key(&self) -> String {
        crypto::encode(&self.public_key)
    }

    pub(crate) fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        let signature = self.key.sign(message);
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(signature.as_ref());
        bytes
    }

    #[cfg(windows)]
    fn from_stored(stored: &[u8]) -> anyhow::Result<Self> {
        Self::from_pkcs8(&unprotect(stored)?)
    }

    #[cfg(unix)]
    fn from_stored(stored: &[u8]) -> anyhow::Result<Self> {
        Self::from_pkcs8(stored)
    }

    fn from_pkcs8(pkcs8: &[u8]) -> anyhow::Result<Self> {
        let key = signature::Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| anyhow::anyhow!("installation identity is damaged"))?;
        let public_key: [u8; 32] = key
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("installation identity is damaged"))?;
        let installation_id = digest::digest(&digest::SHA256, &public_key)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            key,
            public_key,
            installation_id,
        })
    }
}

fn read_stored(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                anyhow::Error::new(error).context("could not inspect the installation identity")
            );
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("installation identity path is not a regular file");
    }
    if metadata.len() > MAX_FILE_BYTES {
        bail!("installation identity is too large");
    }
    let mut stored = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .context("could not read the installation identity")?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut stored)
        .context("could not read the installation identity")?;
    if stored.is_empty() || stored.len() as u64 > MAX_FILE_BYTES {
        bail!("installation identity is damaged");
    }
    Ok(Some(stored))
}

// Publishes through a same-directory temp file and a hard link, so the final
// name appears complete and only when no earlier start created it.
fn publish(path: &Path, plain: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("installation identity directory is missing")?;
    std::fs::create_dir_all(parent).context("could not create the Fintwind data directory")?;
    let temp = parent.join(format!(".{FILE_NAME}.{}.tmp", Uuid::new_v4()));
    let stored = protect(plain)?;
    let written = write_private(&temp, &stored);
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return written;
    }
    let linked = std::fs::hard_link(&temp, path);
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(anyhow::Error::new(error).context("could not store the installation identity"))
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("could not create the installation identity")?;
    file.write_all(bytes)
        .context("could not write the installation identity")?;
    file.sync_all()
        .context("could not flush the installation identity")
}

#[cfg(unix)]
fn protect(plain: &[u8]) -> anyhow::Result<Vec<u8>> {
    // A user-only file is the Unix boundary; the private key bytes are stored
    // as PKCS#8 so the same loader handles both platforms.
    Ok(plain.to_vec())
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::ffi::c_void;

    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("could not protect the installation identity");
    }
    let stored = unsafe {
        let stored = if output.pbData.is_null() || output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        let _ = LocalFree(output.pbData as *mut c_void);
        stored
    };
    if stored.is_empty() {
        bail!("could not protect the installation identity");
    }
    Ok(stored)
}

#[cfg(windows)]
fn unprotect(stored: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::ffi::c_void;

    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: stored.len() as u32,
        pbData: stored.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("installation identity is damaged");
    }
    let plain = unsafe {
        let plain = if output.pbData.is_null() || output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        let _ = LocalFree(output.pbData as *mut c_void);
        plain
    };
    if plain.is_empty() {
        bail!("installation identity is damaged");
    }
    Ok(plain)
}
