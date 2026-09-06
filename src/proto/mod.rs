//! Versioned process protocols: the attachment stream (viewers, koh gateways), the control
//! socket (CLI, zor observers) and the private socket/authentication primitives they share.

pub mod attach;
pub mod control;
pub mod socket;
