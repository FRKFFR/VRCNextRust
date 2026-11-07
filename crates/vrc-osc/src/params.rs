//! Avatar-parameter OSC messages (`/avatar/parameters/<name>`).
//!
//! Community-documented: avatar parameters are addressed under
//! `/avatar/parameters/` and carry a single float or int argument
//! (bools are sent as int 0/1 or float; VRChat accepts both).

use rosc::{OscMessage, OscType};

/// Required prefix for avatar-parameter OSC addresses.
pub const AVATAR_PARAMS_PREFIX: &str = "/avatar/parameters/";

/// A parameter value storable in app state and sendable over OSC.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    /// 32-bit float parameter.
    Float(f32),
    /// 32-bit integer parameter.
    Int(i32),
    /// Boolean parameter (sent as OSC `T`/`F`).
    Bool(bool),
}

impl ParamValue {
    /// The OSC type tag this value encodes as.
    pub fn osc_type(self) -> OscType {
        match self {
            Self::Float(v) => OscType::Float(v),
            Self::Int(v) => OscType::Int(v),
            Self::Bool(v) => OscType::Bool(v),
        }
    }
}

/// `true` for a concrete `/avatar/parameters/<name>` address:
/// the documented prefix, a non-empty suffix, no whitespace and no
/// control characters (OSC strings are NUL-terminated; whitespace makes
/// the address ambiguous).
pub fn valid_param_address(addr: &str) -> bool {
    let Some(name) = addr.strip_prefix(AVATAR_PARAMS_PREFIX) else {
        return false;
    };
    !name.is_empty() && !addr.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Build the OSC message for one avatar parameter.
///
/// Returns `None` when `addr` fails [`valid_param_address`].
pub fn param_message(addr: &str, value: ParamValue) -> Option<OscMessage> {
    valid_param_address(addr).then(|| OscMessage {
        addr: addr.to_string(),
        args: vec![value.osc_type()],
    })
}

/// Extract the latest parameter `(address, value)` from a received OSC
/// message: a `/avatar/parameters/<name>` address carrying exactly one
/// float/int/bool argument. Anything else returns `None`.
pub fn param_value_of(message: &OscMessage) -> Option<(String, ParamValue)> {
    if !valid_param_address(&message.addr) || message.args.len() != 1 {
        return None;
    }
    let value = match message.args[0] {
        OscType::Float(v) => ParamValue::Float(v),
        OscType::Int(v) => ParamValue::Int(v),
        OscType::Bool(v) => ParamValue::Bool(v),
        _ => return None,
    };
    Some((message.addr.clone(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_validation() {
        assert!(valid_param_address("/avatar/parameters/Speed"));
        assert!(valid_param_address("/avatar/parameters/VRCEmote"));
        assert!(valid_param_address("/avatar/parameters/a-b_c.d/e"));
        for bad in [
            "",
            "/avatar/parameters/",
            "/chatbox/input",
            "/avatar/parameters",
            "/avatar/parameters/has space",
            "/avatar/parameters/tab\there",
            "/avatar/parameters/nul\u{0}byte",
        ] {
            assert!(!valid_param_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn param_message_shape() {
        let msg = param_message("/avatar/parameters/Speed", ParamValue::Float(0.75)).unwrap();
        assert_eq!(msg.addr, "/avatar/parameters/Speed");
        assert_eq!(msg.args, vec![OscType::Float(0.75)]);

        let msg = param_message("/avatar/parameters/GestureLeft", ParamValue::Int(1)).unwrap();
        assert_eq!(msg.args, vec![OscType::Int(1)]);

        let msg = param_message("/avatar/parameters/IsOn", ParamValue::Bool(true)).unwrap();
        assert_eq!(msg.args, vec![OscType::Bool(true)]);

        assert!(param_message("/nope", ParamValue::Int(0)).is_none());
    }
}
