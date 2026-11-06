//! Finds the private keys in a folder, so that nobody has to remember their paths.
use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

/// The keys a host without identity files logs in with, in the order they are tried.
const USUAL_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];
/// More than enough for the header of any key.
const HEADER: u64 = 64;

/// The private keys directly in `folder`: the usual ones first, then by name. A folder that cannot
/// be read has none.
pub fn find_ssh_keys(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut keys: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_private_key(path))
        .collect();
    keys.sort_by_key(|path| {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let usual = name
            .as_deref()
            .and_then(|name| USUAL_KEYS.iter().position(|key| *key == name));
        (usual.unwrap_or(USUAL_KEYS.len()), name)
    });
    keys
}

/// By what the file starts with, since a key can be called anything.
fn is_private_key(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut header = String::new();
    // A file that is not text fails here, and is no key we could load.
    if file.take(HEADER).read_to_string(&mut header).is_err() {
        return false;
    }
    let first = header.lines().next().unwrap_or_default();
    first.starts_with("-----BEGIN") && first.contains("PRIVATE KEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("shcp-keys-{name}-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, name: &str, text: &[u8]) {
            fs::write(self.0.join(name), text).unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const OPENSSH: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n";
    const PEM: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----\nabc\n";

    fn names(dir: &Scratch) -> Vec<String> {
        find_ssh_keys(&dir.0)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn only_private_keys_are_listed_with_the_usual_ones_first() {
        let dir = Scratch::new("listed");
        dir.write("zeta", OPENSSH);
        dir.write("alpha.pem", PEM);
        dir.write("id_rsa", PEM);
        dir.write("id_ed25519", OPENSSH);
        dir.write("id_ed25519.pub", b"ssh-ed25519 AAAA me@pc\n");
        dir.write("known_hosts", b"host ssh-ed25519 AAAA\n");
        dir.write("config", b"Host *\n");
        dir.write("binary", &[0xff, 0xfe, 0x00, 0x01]);
        fs::create_dir_all(dir.0.join("folder")).unwrap();
        assert_eq!(names(&dir), ["id_ed25519", "id_rsa", "alpha.pem", "zeta"]);
    }

    #[test]
    fn a_missing_folder_has_no_keys() {
        assert!(find_ssh_keys(Path::new("/no/such/folder/at/all")).is_empty());
    }
}
