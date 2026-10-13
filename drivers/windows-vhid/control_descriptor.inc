// HID report descriptor of the control collection of each coroom virtual gamepad.
//
// The driver appends it to report_descriptor.inc (coroomvhid.c), so every gamepad device has two
// top-level collections: the gamepad (Col01) and this one (Col02). It is vendor-defined, so games,
// DirectInput, SDL and Steam ignore it; coroom finds it by its usage page and usage
// (COROOM_VHID_CONTROL_USAGE_PAGE and COROOM_VHID_CONTROL_USAGE in coroom_vhid.h). The Rust client
// parses this file in its tests too. Only hexadecimal bytes, commas and comments are allowed here.
//
// Output report 2 (17 bytes including the report ID): a whole gamepad input report (report ID 1
//   included), which becomes the new state of the gamepad.
// Feature report 3 (9 bytes including the report ID, read only): COROOM_VHID_INFO (protocol
//   version, slot, size of the gamepad report).
0x06, 0xC0, 0xFF, // Usage Page (Vendor Defined 0xFFC0)
0x09, 0x01,       // Usage (Vendor 0x01)
0xA1, 0x01,       // Collection (Application)
0x85, 0x02,       //   Report ID (2)
0x09, 0x02,       //   Usage (Vendor 0x02): gamepad state
0x15, 0x00,       //   Logical Minimum (0)
0x26, 0xFF, 0x00, //   Logical Maximum (255)
0x75, 0x08,       //   Report Size (8)
0x95, 0x10,       //   Report Count (16)
0x91, 0x02,       //   Output (Data, Variable, Absolute)
0x85, 0x03,       //   Report ID (3)
0x09, 0x03,       //   Usage (Vendor 0x03): info
0x95, 0x08,       //   Report Count (8)
0xB1, 0x02,       //   Feature (Data, Variable, Absolute)
0xC0,             // End Collection
