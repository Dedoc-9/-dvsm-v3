// tools/hash_manifest.rs
// Build reproducibility: SHA-256 over source + shader + config.
// Run before benchmarking. Any hash mismatch = dirty build = invalid claim.
//
// Math note: we use a Merkle-like structure:
//   manifest_hash = H(source_hash || shader_hash || config_hash || git_commit)
// Collision probability under SHA-256: ~2^{-128} per pair. Negligible.

use sha2::{Sha256, Digest};

pub struct BuildManifest {
    pub git_commit:   String,
    pub source_hash:  String,
    pub shader_hash:  String,
    pub config_hash:  String,
    pub manifest_hash: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

impl BuildManifest {
    pub fn compute(
        git_commit: &str,
        source: &[u8],
        shader: &[u8],
        config: &[u8],
    ) -> Self {
        let sh = sha256_hex(source);
        let wh = sha256_hex(shader);
        let ch = sha256_hex(config);
        let combined = [sh.as_bytes(), wh.as_bytes(), ch.as_bytes(),
                        git_commit.as_bytes()].concat();
        let mh = sha256_hex(&combined);
        Self {
            git_commit: git_commit.to_string(),
            source_hash: sh,
            shader_hash: wh,
            config_hash: ch,
            manifest_hash: mh,
        }
    }

    pub fn print(&self) {
        println!("DVSM-v3 Build Manifest");
        println!("  git:     {}", &self.git_commit[..8.min(self.git_commit.len())]);
        println!("  source:  {}", &self.source_hash[..16]);
        println!("  shader:  {}", &self.shader_hash[..16]);
        println!("  config:  {}", &self.config_hash[..16]);
        println!("  TOTAL:   {}", self.manifest_hash);
    }
}
