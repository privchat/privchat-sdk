use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    // Note: We're using proc-macro mode (setup_scaffolding!),
    // so we don't need to generate scaffolding from UDL file.
    // The UDL file is kept for namespace-level functions only.
    
    // Still track UDL file changes for namespace functions
    println!("cargo:rerun-if-changed=src/api.udl");
    
    Ok(())
}
