// Reads the largest transcript and renders last 3 prompts; prints only sizes.
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let bytes = std::fs::read(&path).expect("read");
    let text = String::from_utf8_lossy(&bytes);
    let turns = transcript::parse(&text);
    let out = transcript::render_brief(transcript::last_prompts(&turns, 3));
    let full = transcript::render_full(transcript::last_prompts(&turns, 100));
    println!("bytes={} turns={} brief_len={} full100_len={}", bytes.len(), turns.len(), out.len(), full.len());
}
