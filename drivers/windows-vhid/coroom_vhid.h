/*
 * coroom_vhid.h - public interface of the coroom virtual gamepad driver (coroomvhid.sys).
 *
 * Shared by the driver and its user-mode clients. The Rust client
 * (crates/input/src/inject/windows_vhid.rs) does not compile this header: it mirrors every value
 * below and its tests parse this file to check that both sides agree, so keep the
 * "#define NAME VALUE" lines simple (one decimal or hexadecimal literal, or COROOM_VHID_IOCTL(n)).
 *
 * Include it after <wdm.h>/<ntddk.h> (kernel) or <windows.h>/<winioctl.h> (user mode), which
 * define CTL_CODE and the fixed-size integer types.
 */

#ifndef COROOM_VHID_H
#define COROOM_VHID_H

/* Version of this interface; IOCTL_COROOM_VHID_VERSION returns it. Bump on any change below. */
#define COROOM_VHID_PROTOCOL_VERSION 1

/* Device names: kernel object and the symbolic link that user mode opens as \\.\coroomvhid. */
#define COROOM_VHID_NT_DEVICE_NAME L"\\Device\\coroomvhid"
#define COROOM_VHID_DOS_DEVICE_NAME L"\\DosDevices\\coroomvhid"
#define COROOM_VHID_USER_PATH L"\\\\.\\coroomvhid"

/*
 * Access to the control device: full access for SYSTEM, administrators and the interactive user;
 * nobody else can open it (services running as other accounts, network logons).
 */
#define COROOM_VHID_SDDL L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)"

/* Virtual gamepads (slots 0 to COROOM_VHID_MAX_SLOTS - 1). */
#define COROOM_VHID_MAX_SLOTS 8

/*
 * USB identifiers that the virtual gamepads report. The gamepads are virtual, so there is no
 * vendor or product to claim: both are 0. SDL (and Steam Input on top of it) then identify the
 * controller by its name and version instead of VID/PID, so it never collides with another device
 * (see drivers/windows-vhid/README.md, "Identificadores USB"). A distributed driver would use a PID
 * registered at pid.codes instead.
 */
#define COROOM_VHID_VENDOR_ID 0x0000
#define COROOM_VHID_PRODUCT_ID 0x0000
#define COROOM_VHID_VERSION_NUMBER 0x0100

/* Input report (report_descriptor.inc): 16 bytes, the first one is the report ID. */
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

/*
 * IOCTLs (METHOD_BUFFERED, FILE_ANY_ACCESS: who may open the device is decided by
 * COROOM_VHID_SDDL). Every buffer size must match exactly, except the output of
 * IOCTL_COROOM_VHID_VERSION, which may be larger.
 *
 *   IOCTL_COROOM_VHID_VERSION  in: nothing                     out: COROOM_VHID_VERSION_INFO
 *   IOCTL_COROOM_VHID_CREATE   in: COROOM_VHID_SLOT            out: nothing
 *   IOCTL_COROOM_VHID_DESTROY  in: COROOM_VHID_SLOT            out: nothing
 *   IOCTL_COROOM_VHID_REPORT   in: COROOM_VHID_REPORT_PACKET   out: nothing
 *
 * A slot belongs to the handle that created it: other handles get STATUS_ACCESS_DENIED (CREATE:
 * STATUS_DEVICE_BUSY) and closing that handle destroys its slots. Errors: STATUS_INVALID_PARAMETER
 * (slot out of range or malformed report), STATUS_INVALID_BUFFER_SIZE (wrong size),
 * STATUS_INVALID_DEVICE_STATE (slot not created), STATUS_DEVICE_REMOVED (the root device is being
 * removed) and STATUS_INVALID_DEVICE_REQUEST (unknown IOCTL).
 */
#define COROOM_VHID_IOCTL(n) CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800 + (n), METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_COROOM_VHID_VERSION COROOM_VHID_IOCTL(0)
#define IOCTL_COROOM_VHID_CREATE COROOM_VHID_IOCTL(1)
#define IOCTL_COROOM_VHID_DESTROY COROOM_VHID_IOCTL(2)
#define IOCTL_COROOM_VHID_REPORT COROOM_VHID_IOCTL(3)

#define COROOM_VHID_VERSION_INFO_SIZE 8
#define COROOM_VHID_SLOT_SIZE 1
#define COROOM_VHID_REPORT_PACKET_SIZE 17

#pragma pack(push, 1)

/* Output of IOCTL_COROOM_VHID_VERSION. */
typedef struct _COROOM_VHID_VERSION_INFO {
    UINT32 ProtocolVersion; /* COROOM_VHID_PROTOCOL_VERSION */
    UINT8 MaxSlots;         /* COROOM_VHID_MAX_SLOTS */
    UINT8 ReportSize;       /* COROOM_VHID_REPORT_SIZE */
    UINT16 Reserved;        /* zero */
} COROOM_VHID_VERSION_INFO, *PCOROOM_VHID_VERSION_INFO;

/* Input of IOCTL_COROOM_VHID_CREATE and IOCTL_COROOM_VHID_DESTROY. */
typedef struct _COROOM_VHID_SLOT {
    UINT8 Slot; /* 0 .. COROOM_VHID_MAX_SLOTS - 1 */
} COROOM_VHID_SLOT, *PCOROOM_VHID_SLOT;

/* Input of IOCTL_COROOM_VHID_REPORT. */
typedef struct _COROOM_VHID_REPORT_PACKET {
    UINT8 Slot;                            /* 0 .. COROOM_VHID_MAX_SLOTS - 1 */
    UINT8 Report[COROOM_VHID_REPORT_SIZE]; /* Report[0] == COROOM_VHID_REPORT_ID */
} COROOM_VHID_REPORT_PACKET, *PCOROOM_VHID_REPORT_PACKET;

#pragma pack(pop)

/* C_ASSERT comes from the WDK and the Windows SDK (winnt.h / ntdef.h). */
C_ASSERT(sizeof(COROOM_VHID_VERSION_INFO) == COROOM_VHID_VERSION_INFO_SIZE);
C_ASSERT(sizeof(COROOM_VHID_SLOT) == COROOM_VHID_SLOT_SIZE);
C_ASSERT(sizeof(COROOM_VHID_REPORT_PACKET) == COROOM_VHID_REPORT_PACKET_SIZE);

#endif /* COROOM_VHID_H */
