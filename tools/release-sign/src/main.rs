//! Update signing for SaveSync releases (Ed25519).
//!
//!   release-sign keygen <key-file>          create a key (private, 0600) and print the public key
//!   release-sign pubkey <key-file>          print the public key (hex) to embed in the apps
//!   release-sign sign <key-file> <file>     write <file>.sig (hex signature of the file's bytes)
//!   release-sign verify <pubkey> <file>     check <file>.sig against a hex public key
//!
//! The private key never leaves the release machine. Back it up: without it,
//! installed apps won't accept updates signed by a new key.

use std::{fs, path::Path, process::exit};

use ring::{
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};

fn die(msg: &str) -> ! {
    eprintln!("release-sign: {msg}");
    exit(1)
}

fn load(key_file: &str) -> Ed25519KeyPair {
    let pkcs8 = fs::read(key_file).unwrap_or_else(|e| die(&format!("can't read {key_file}: {e}")));
    Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap_or_else(|_| die("not an Ed25519 PKCS#8 key"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", key_file] => {
            if Path::new(key_file).exists() {
                die(&format!("{key_file} already exists; refusing to overwrite a signing key"));
            }
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap_or_else(|_| die("keygen failed"));
            if let Some(dir) = Path::new(key_file).parent() {
                fs::create_dir_all(dir).unwrap_or_else(|e| die(&e.to_string()));
            }
            fs::write(key_file, pkcs8.as_ref()).unwrap_or_else(|e| die(&e.to_string()));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(key_file, fs::Permissions::from_mode(0o600)).unwrap_or_else(|e| die(&e.to_string()));
            }
            println!("{}", hex(load(key_file).public_key().as_ref()));
        }
        ["pubkey", key_file] => println!("{}", hex(load(key_file).public_key().as_ref())),
        ["sign", key_file, file] => {
            let data = fs::read(file).unwrap_or_else(|e| die(&format!("can't read {file}: {e}")));
            let sig = load(key_file).sign(&data);
            fs::write(format!("{file}.sig"), hex(sig.as_ref()) + "\n").unwrap_or_else(|e| die(&e.to_string()));
        }
        ["verify", pubkey, file] => {
            let key = unhex(pubkey).unwrap_or_else(|| die("bad public key"));
            let data = fs::read(file).unwrap_or_else(|e| die(&e.to_string()));
            let sig = fs::read_to_string(format!("{file}.sig")).unwrap_or_else(|e| die(&e.to_string()));
            let sig = unhex(sig.trim()).unwrap_or_else(|| die("bad signature file"));
            match UnparsedPublicKey::new(&ED25519, key).verify(&data, &sig) {
                Ok(()) => println!("OK"),
                Err(_) => die("signature does NOT match"),
            }
        }
        _ => die("usage: keygen <key> | pubkey <key> | sign <key> <file> | verify <pubkey-hex> <file>"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
