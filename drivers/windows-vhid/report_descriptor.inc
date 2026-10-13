// HID report descriptor of the coroom virtual gamepad (one per slot): the first top-level
// collection (Col01) of each device, followed by control_descriptor.inc (Col02).
//
// Single source of truth: the driver includes this file inside a C array initializer
// (coroomvhid.c) and the Rust client parses it in its tests (crates/input/src/inject/
// windows_vhid.rs), which check that the report layout in coroom_vhid.h matches it.
// Only hexadecimal bytes, commas and comments are allowed here.
//
// Input report 1 (16 bytes including the report ID):
//   byte  0      report ID (1)
//   bytes 1-2    16 buttons, one bit each (button 1 = bit 0 of byte 1)
//   bytes 3-10   X, Y, Rx, Ry: sticks, int16 little endian, -32767..32767 (Y down positive)
//   bytes 11-14  Z, Rz: left and right triggers, int16 little endian, 0..32767
//   byte  15     hat switch in bits 0-3 (0 = up, clockwise in 45 degree steps, 8 = centred)
//                and 4 bits of padding
// No output or feature reports (no rumble).
0x05, 0x01,       // Usage Page (Generic Desktop)
0x09, 0x05,       // Usage (Game Pad)
0xA1, 0x01,       // Collection (Application)
0x85, 0x01,       //   Report ID (1)
0x05, 0x09,       //   Usage Page (Button)
0x19, 0x01,       //   Usage Minimum (Button 1)
0x29, 0x10,       //   Usage Maximum (Button 16)
0x15, 0x00,       //   Logical Minimum (0)
0x25, 0x01,       //   Logical Maximum (1)
0x75, 0x01,       //   Report Size (1)
0x95, 0x10,       //   Report Count (16)
0x81, 0x02,       //   Input (Data, Variable, Absolute)
0x05, 0x01,       //   Usage Page (Generic Desktop)
0x09, 0x30,       //   Usage (X)
0x09, 0x31,       //   Usage (Y)
0x09, 0x33,       //   Usage (Rx)
0x09, 0x34,       //   Usage (Ry)
0x16, 0x01, 0x80, //   Logical Minimum (-32767)
0x26, 0xFF, 0x7F, //   Logical Maximum (32767)
0x75, 0x10,       //   Report Size (16)
0x95, 0x04,       //   Report Count (4)
0x81, 0x02,       //   Input (Data, Variable, Absolute)
0x09, 0x32,       //   Usage (Z)
0x09, 0x35,       //   Usage (Rz)
0x15, 0x00,       //   Logical Minimum (0)
0x26, 0xFF, 0x7F, //   Logical Maximum (32767)
0x95, 0x02,       //   Report Count (2)
0x81, 0x02,       //   Input (Data, Variable, Absolute)
0x09, 0x39,       //   Usage (Hat Switch)
0x15, 0x00,       //   Logical Minimum (0)
0x25, 0x07,       //   Logical Maximum (7)
0x35, 0x00,       //   Physical Minimum (0)
0x46, 0x3B, 0x01, //   Physical Maximum (315)
0x65, 0x14,       //   Unit (English Rotation: degrees)
0x75, 0x04,       //   Report Size (4)
0x95, 0x01,       //   Report Count (1)
0x81, 0x42,       //   Input (Data, Variable, Absolute, Null State)
0x65, 0x00,       //   Unit (None)
0x45, 0x00,       //   Physical Maximum (0)
0x81, 0x03,       //   Input (Constant, Variable, Absolute): 4 bits of padding
0xC0,             // End Collection
