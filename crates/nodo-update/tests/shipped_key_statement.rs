//! The key statement published with every stable snapshot (feed/snapshot-keys/stable) must be one
//! that a device with the shipped pins applies without changing what it trusts.

use nodo_update::trust::{Pins, TrustState};
use nodo_update::KeyStatement;

const PINS: &str = include_str!("../../../packages/nodo-tools/files/snapshot-pins.toml");
const STATEMENT: &[u8] = include_bytes!("../../../feed/snapshot-keys/stable/keys.toml");
const SIGNATURES: &[u8] = include_bytes!("../../../feed/snapshot-keys/stable/keys.toml.sig");

#[test]
fn the_published_key_statement_is_signed_by_the_pinned_keys_and_changes_no_trust() {
    let pins: Pins = toml::from_str(PINS).unwrap();
    let mut state = TrustState::from_pins(&pins);
    state.apply_key_statement(STATEMENT, SIGNATURES).unwrap();
    assert_eq!(state.keys, pins.keys);
    assert_eq!(state.threshold, pins.threshold);
    assert_eq!(state.key_sequence, 1);
    // Applied once, it is stale, which the client skips without reading its signatures.
    assert!(state.apply_key_statement(STATEMENT, SIGNATURES).is_err());
    assert_eq!(KeyStatement::parse(STATEMENT).unwrap().sequence, 1);
}
