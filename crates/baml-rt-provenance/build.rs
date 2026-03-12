//! Build script: compile GraphQLite extension from submodule and copy to target dir.
//! At runtime, we set GRAPHQLITE_EXTENSION_PATH to the extension next to the binary.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.ancestors().nth(2).unwrap();

    // Check: GRAPHQLITE_REPO_PATH, submodule, or sibling clone
    let graphqlite_root = env::var("GRAPHQLITE_REPO_PATH")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.join("Makefile").exists())
        .or_else(|| {
            let submodule = workspace_root.join("graphqlite");
            if submodule.join("Makefile").exists() {
                Some(submodule)
            } else {
                let sibling = workspace_root.join("..").join("graphqlite");
                if sibling.join("Makefile").exists() {
                    Some(sibling)
                } else {
                    None
                }
            }
        });

    let Some(graphqlite_root) = graphqlite_root else {
        // No graphqlite repo; rely on GRAPHQLITE_EXTENSION_PATH at runtime
        return;
    };

    // Build extension
    let output = Command::new("make")
        .arg("extension")
        .arg("RELEASE=1")
        .current_dir(&graphqlite_root)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "cargo:warning=Could not run make in graphqlite: {e}. Set GRAPHQLITE_EXTENSION_PATH manually."
            );
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "cargo:warning=make extension failed in graphqlite. Set GRAPHQLITE_EXTENSION_PATH manually."
        );
        eprintln!("cargo:warning=Build output:\n{stderr}");
        if stderr.contains("syntax error") && stderr.contains(".y") {
            eprintln!(
                "cargo:warning=Hint: GraphQLite requires Bison 3.x. On macOS: brew install bison"
            );
        }
        return;
    }

    // Extension output: build/graphqlite.dylib (macOS), build/graphqlite.so (Linux), build/graphqlite.dll (Windows)
    let ext_name = if cfg!(target_os = "macos") {
        "graphqlite.dylib"
    } else if cfg!(target_os = "linux") {
        "graphqlite.so"
    } else if cfg!(target_os = "windows") {
        "graphqlite.dll"
    } else {
        eprintln!("cargo:warning=Unsupported target for GraphQLite extension");
        return;
    };

    let built_ext = graphqlite_root.join("build").join(ext_name);
    if !built_ext.exists() {
        eprintln!(
            "cargo:warning=Extension not found at {:?}. Set GRAPHQLITE_EXTENSION_PATH manually.",
            built_ext
        );
        return;
    }

    // Copy to target/debug or target/release so it sits next to the binary
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_dir = out_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("OUT_DIR should be under target/");

    let dest = target_dir.join(ext_name);
    if let Err(e) = fs::copy(&built_ext, &dest) {
        eprintln!(
            "cargo:warning=Could not copy extension to {:?}: {}. Set GRAPHQLITE_EXTENSION_PATH manually.",
            dest, e
        );
        return;
    }

    println!(
        "cargo:rerun-if-changed={}/Makefile",
        graphqlite_root.display()
    );
    println!("cargo:rerun-if-changed={}/src", graphqlite_root.display());
}
