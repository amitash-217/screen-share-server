//! Everything in the desktop client that does not need a window: server
//! addresses, networking, audio, and the state the screens render.

pub mod addr;
pub mod audio;
pub mod capture;
pub mod config;
pub mod model;
pub mod net;
pub mod rtc;
pub mod video;
