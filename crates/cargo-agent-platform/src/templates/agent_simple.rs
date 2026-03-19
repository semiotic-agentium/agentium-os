//! Simple agent template — basic agent with or without tools.
//!
//! This template wraps the existing `run_bootstrap` from baml-rt-builder.
//! For simple and basic-tools templates, we delegate to the bootstrap logic.

/// Generate the canonical tsconfig.json content.
pub fn generate_tsconfig() -> String {
    r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "noEmit": true
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist"]
}
"#
    .to_string()
}
