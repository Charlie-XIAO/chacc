use std::hash::Hasher;
use std::path::Path;

use xxhash_rust::xxh3::Xxh3;

fn main() {
    let root_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let include_dir = root_dir.join("include");
    println!("cargo:rerun-if-changed={}", include_dir.display());

    let mut hash = Xxh3::new();
    for header in [
        "float.h",
        "stdalign.h",
        "stdarg.h",
        "stdbool.h",
        "stddef.h",
        "stdnoreturn.h",
    ] {
        let path = include_dir.join(header);
        println!("cargo:rerun-if-changed={}", path.display());

        hash.write(b"path");
        hash.write_u64(header.len() as _);
        hash.write(header.as_bytes());

        let content = std::fs::read(&path).unwrap();
        hash.write(b"content");
        hash.write_u64(content.len() as _);
        hash.write(&content);
    }

    let hash = hash.finish();
    println!("cargo:rustc-env=BUILTIN_INCLUDE_HEADERS_HASH={hash:016x}")
}
