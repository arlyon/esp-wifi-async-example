@0xf57b27a4a21cb930;

struct Measurement {
    timeSince @0 :UInt32;
    measurement :union {
        temperature @1 :Float32;
        humidity @2 :Float32;
        co2 @3 :UInt16;
    }
}

struct Measurements {
    measurements @0 :List(Measurement);
}