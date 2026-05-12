mod bus_event;

pub use bus_event::{
    BusEvent, BusEventKind, BusScope, MAX_MESSAGE_BYTES, SCHEMA_VERSION, TruncatedMessage,
    UnknownBusEventKind, truncate_message,
};
