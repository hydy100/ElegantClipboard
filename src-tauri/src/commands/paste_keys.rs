// Win32 virtual-key codes. Kept separate from SendInput for deterministic tests.
pub(super) fn combo(value: &str) -> (u16, u16) {
    if value == "shift_insert" { (0x10, 0x2D) } else { (0x11, 0x56) }
}

pub(super) fn send_combo(
    value: &str,
    modifier_held: bool,
    mut send: impl FnMut(u16, bool) -> Result<(), String>,
    wait: impl FnOnce(),
) -> Result<(), String> {
    let (modifier, key) = combo(value);
    if !modifier_held { send(modifier, false)?; }
    let pressed = send(key, false);
    if pressed.is_ok() { wait(); }
    // Always try both releases, even when key-down or key-up fails.
    let released = send(key, true);
    let modifier_released = if modifier_held { Ok(()) } else { send(modifier, true) };
    pressed.and(released).and(modifier_released)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_unknown_and_insert_use_expected_keys() {
        for (setting, modifier, key) in [("ctrl_v", 0x11, 0x56), ("unknown", 0x11, 0x56), ("shift_insert", 0x10, 0x2D)] {
            let mut events = Vec::new();
            send_combo(setting, false, |k, up| { events.push((k, up)); Ok(()) }, || {}).unwrap();
            assert_eq!(events, [(modifier, false), (key, false), (key, true), (modifier, true)]);
        }
    }
    #[test]
    fn failed_key_down_still_releases_injected_keys() {
        let mut events = Vec::new();
        assert!(send_combo("shift_insert", false, |k, up| {
            events.push((k, up));
            if k == 0x2D && !up { Err("injected failure".into()) } else { Ok(()) }
        }, || panic!("must not wait after failed key-down")).is_err());
        assert_eq!(events, [(0x10, false), (0x2D, false), (0x2D, true), (0x10, true)]);
    }
    #[test]
    fn held_modifier_is_not_released() {
        let mut events = Vec::new();
        send_combo("ctrl_v", true, |k, up| { events.push((k, up)); Ok(()) }, || {}).unwrap();
        assert_eq!(events, [(0x56, false), (0x56, true)]);
    }
}
