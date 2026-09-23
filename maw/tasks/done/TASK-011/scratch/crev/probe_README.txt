code-reviewer probes. Append probe_registry_tests.rs (without the final closing brace) into the
`mod tests` of crates/cctg/src/hub/registry.rs of a copy of HEAD 9457d7e and run:
  cargo test -p cctg --offline --lib -- crev_ --nocapture
Result 2026-09-23: both FAILED (see probe_out.txt).
