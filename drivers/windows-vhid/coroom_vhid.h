/*
 * coroom_vhid.h - public interface of the coroom virtual gamepad driver (coroomvhid.dll, UMDF2).
 *
 * Shared by the driver and its user-mode clients. The Rust client
 * (crates/input/src/inject/windows_vhid.rs) does not compile this header: it mirrors every value
 * below and its tests parse this file to check that both sides agree, so keep the
 * "#define NAME VALUE" lines simple (one decimal or hexadecimal literal, or a L"..." string).
 *
 * Model (protocol 2): every slot is its own root-enumerated device (Root\coroomvhid, created by
 * install.ps1) whose UMDF2 HID minidriver runs under the inbox mshidumdf.sys. Its report
 * descriptor has two top-level collections, so hidclass creates two HID devices per slot:
 *
 *   Col01  the gamepad (report_descriptor.inc): what games, Steam and SDL see.
 *   Col02  the control collection (control_descriptor.inc), vendor-defined, through which coroom
 *          feeds the gamepad: an output report with the whole gamepad input report and a feature
 *          report that identifies the slot and the protocol.
 *
 * A HID minidriver under mshidumdf cannot expose a control device or private IOCTLs (hidclass owns
 * the top of the stack), so the client talks to it only through HID reports: WriteFile or
 * HidD_SetOutputReport on Col02 for the state, HidD_GetFeature for the identification.
 *
 * Include it after <windows.h> (or the WDF headers), which define the fixed-size integer types.
 */

#ifndef COROOM_VHID_H
#define COROOM_VHID_H

/* Version of this interface; the info feature report returns it. Bump on any change below. */
#define COROOM_VHID_PROTOCOL_VERSION 2

/* Virtual gamepads: slot numbers 0 to COROOM_VHID_MAX_SLOTS - 1, one root device each. */
#define COROOM_VHID_MAX_SLOTS 8

/*
 * Hardware ID of the root devices (coroomvhid.inf) and the DWORD value of their hardware key
 * ("Device Parameters") that tells each one its slot; install.ps1 writes it when it creates the
 * device and the driver refuses to start without it.
 */
#define COROOM_VHID_HARDWARE_ID L"Root\\coroomvhid"
#define COROOM_VHID_SLOT_VALUE L"CoroomSlot"

/*
 * USB identifiers that the virtual gamepads report. They are virtual, so there is no vendor or
 * product to claim: both are 0 and SDL (and Steam Input on top of it) identify the controller by
 * its name and version instead, so it never collides with a real device. Moving to a PID of
 * pid.codes is described in drivers/windows-vhid/README.md ("Identificadores USB"). The client
 * finds the control collection by its vendor usage, not by these.
 */
#define COROOM_VHID_VENDOR_ID 0x0000
#define COROOM_VHID_PRODUCT_ID 0x0000
#define COROOM_VHID_VERSION_NUMBER 0x0100

/* HID strings (the product string is the name Steam, SDL and joy.cpl show). */
#define COROOM_VHID_MANUFACTURER L"coroom"
#define COROOM_VHID_PRODUCT L"Mando virtual de coroom"

/* Gamepad input report (report_descriptor.inc): 16 bytes, the first one is the report ID. */
#define COROOM_VHID_REPORT_ID 1
#define COROOM_VHID_REPORT_SIZE 16
#define COROOM_VHID_REPORT_OFFSET_BUTTONS 1  /* UINT16 LE, button n (1..16) = bit n - 1 */
#define COROOM_VHID_REPORT_OFFSET_X 3        /* INT16 LE, left stick, -32767..32767 */
#define COROOM_VHID_REPORT_OFFSET_Y 5        /* INT16 LE, left stick, down is positive */
#define COROOM_VHID_REPORT_OFFSET_RX 7       /* INT16 LE, right stick */
#define COROOM_VHID_REPORT_OFFSET_RY 9       /* INT16 LE, right stick, down is positive */
#define COROOM_VHID_REPORT_OFFSET_Z 11       /* INT16 LE, left trigger, 0..32767 */
#define COROOM_VHID_REPORT_OFFSET_RZ 13      /* INT16 LE, right trigger, 0..32767 */
#define COROOM_VHID_REPORT_OFFSET_HAT 15     /* bits 0-3: hat, bits 4-7: zero */
#define COROOM_VHID_HAT_NULL 8               /* hat centred; 0 = up, clockwise, 7 = up-left */
#define COROOM_VHID_AXIS_MAX 32767

/* Control collection (control_descriptor.inc): vendor-defined usage page and usage. */
#define COROOM_VHID_CONTROL_USAGE_PAGE 0xFFC0
#define COROOM_VHID_CONTROL_USAGE 0x0001

/*
 * State output report: the report ID followed by a whole gamepad input report (which starts with
 * COROOM_VHID_REPORT_ID). The driver checks it against the descriptor ranges and hands it to
 * hidclass. 17 bytes with the report ID, the OutputReportByteLength of the control collection.
 */
#define COROOM_VHID_STATE_REPORT_ID 2
#define COROOM_VHID_STATE_REPORT_SIZE 17

/*
 * Info feature report (read only): the report ID followed by COROOM_VHID_INFO. 9 bytes with the
 * report ID, the FeatureReportByteLength of the control collection.
 */
#define COROOM_VHID_INFO_REPORT_ID 3
#define COROOM_VHID_INFO_REPORT_SIZE 9
#define COROOM_VHID_INFO_SIZE 8

/*
 * Watchdog: without a state report for this long, the driver puts the gamepad back at rest
 * (sticks centred, nothing pressed), so a client that dies does not leave a button held down. A
 * live client repeats its last state well within this time.
 */
#define COROOM_VHID_WATCHDOG_MS 1000

#pragma pack(push, 1)

/* Payload of the info feature report. */
typedef struct _COROOM_VHID_INFO {
    UINT32 ProtocolVersion; /* COROOM_VHID_PROTOCOL_VERSION */
    UINT8 Slot;             /* 0 .. COROOM_VHID_MAX_SLOTS - 1 */
    UINT8 ReportSize;       /* COROOM_VHID_REPORT_SIZE */
    UINT16 Reserved;        /* zero */
} COROOM_VHID_INFO, *PCOROOM_VHID_INFO;

#pragma pack(pop)

/* C_ASSERT comes from the WDK and the Windows SDK (winnt.h). */
C_ASSERT(sizeof(COROOM_VHID_INFO) == COROOM_VHID_INFO_SIZE);
C_ASSERT(COROOM_VHID_INFO_REPORT_SIZE == 1 + COROOM_VHID_INFO_SIZE);
C_ASSERT(COROOM_VHID_STATE_REPORT_SIZE == 1 + COROOM_VHID_REPORT_SIZE);

#endif /* COROOM_VHID_H */
