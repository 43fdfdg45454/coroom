/*
 * coroom_vhid.h - public interface of the coroom virtual gamepad driver (coroomvhid.dll, UMDF2).
 *
 * Shared by the driver and its user-mode clients. The Rust client
 * (crates/input/src/inject/windows_vhid.rs) does not compile this header: it mirrors every value
 * below and its tests parse this file to check that both sides agree, so keep the
 * "#define NAME VALUE" lines simple (one decimal or hexadecimal literal, or a L"..." string).
 *
 * Model (protocol 3): one DLL, two kinds of root device, both created once by install.ps1.
 *
 *   Root\coroomvhid   the control device (one). A HID source driver of the Virtual HID Framework
 *                     (vhf.sys is its lower filter): through its device interface the client asks
 *                     for gamepads and the driver creates each one with VhfCreate, as a HID
 *                     device with the VID/PID, version and report descriptor the client gives
 *                     (the guest's own controller, or the generic gamepad of
 *                     report_descriptor.inc). Up to COROOM_VHID_MAX_PADS at a time. They live as
 *                     long as the handle that created them: closing it (or the client dying)
 *                     removes them.
 *   Root\coroomxusb   COROOM_VHID_XUSB_SLOTS devices, each one an Xbox 360 controller for XInput
 *                     and Windows.Gaming.Input (the xusb22 interface, GUID_DEVINTERFACE_XUSB). Its
 *                     XUSB interface is enabled only while a client has activated it, so games see
 *                     no controller when nobody uses the slot.
 *
 * Both have the private interface GUID_DEVINTERFACE_COROOM_VHID with the IOCTLs below
 * (METHOD_BUFFERED, little-endian structures without padding). The Xbox 360 devices also answer
 * the xusb22 IOCTLs that xinput1_4.dll and Windows.Gaming.Input send.
 *
 * Include it after <windows.h> (or the WDF headers), which define the fixed-size integer types,
 * GUID and CTL_CODE.
 */

#ifndef COROOM_VHID_H
#define COROOM_VHID_H

/* Version of this interface; COROOM_VHID_INFO returns it. Bump on any change below. */
#define COROOM_VHID_PROTOCOL_VERSION 3

/* HID gamepads of the control device: pad numbers 0 to COROOM_VHID_MAX_PADS - 1. */
#define COROOM_VHID_MAX_PADS 8

/* Xbox 360 devices: slots 0 to COROOM_VHID_XUSB_SLOTS - 1 (XInput has four user indexes). */
#define COROOM_VHID_XUSB_SLOTS 4

/* Hardware IDs of the root devices (coroomvhid.inf, install.ps1). */
#define COROOM_VHID_HARDWARE_ID L"Root\\coroomvhid"
#define COROOM_VHID_XUSB_HARDWARE_ID L"Root\\coroomxusb"

/*
 * DWORD value of the hardware key ("Device Parameters") of every Xbox 360 device that tells it its
 * slot; install.ps1 writes it and the device refuses to start without it.
 */
#define COROOM_VHID_SLOT_VALUE L"CoroomSlot"

/* {7C6B2C3E-55D1-4C7A-9C4B-2E0B6F1D8A41}: the private interface of both kinds of device. */
#define COROOM_VHID_INTERFACE_GUID L"{7C6B2C3E-55D1-4C7A-9C4B-2E0B6F1D8A41}"

/* Limits of a gamepad (the same as the wire protocol of coroom, coroom-proto hid and input). */
#define COROOM_VHID_MAX_DESCRIPTOR 4096
#define COROOM_VHID_MAX_REPORT 512
#define COROOM_VHID_MAX_FEATURES 32
#define COROOM_VHID_MAX_FEATURE_BYTES 8192

/*
 * Watchdog: a gamepad created with a rest report (NeutralLength > 0) goes back to it when no input
 * report arrives for this long, and an active Xbox 360 device goes to rest too, so a client that
 * hangs does not leave a button held down. A live client repeats its last report well within it.
 */
#define COROOM_VHID_WATCHDOG_MS 1000

/* Outputs (vibration, LEDs, feature writes) kept for the client until it reads them. */
#define COROOM_VHID_MAX_OUTPUTS 64

/*
 * IOCTLs: CTL_CODE(FILE_DEVICE_UNKNOWN, function, METHOD_BUFFERED,
 * FILE_READ_DATA | FILE_WRITE_DATA), functions 0x900 and up.
 */
#define COROOM_VHID_IOCTL_GET_INFO 0x0022E400        /* both devices; out: COROOM_VHID_INFO */
#define COROOM_VHID_IOCTL_CREATE 0x0022E404          /* control; in: COROOM_VHID_CREATE + data */
#define COROOM_VHID_IOCTL_DESTROY 0x0022E408         /* control; in: UINT8 pad */
#define COROOM_VHID_IOCTL_INPUT 0x0022E40C           /* control; in: COROOM_VHID_INPUT + report */
#define COROOM_VHID_IOCTL_TAKE_OUTPUT 0x0022E410     /* control; out: COROOM_VHID_OUTPUT */
#define COROOM_VHID_IOCTL_XUSB_ACTIVATE 0x0022E440   /* Xbox 360; in: COROOM_VHID_XUSB_ID */
#define COROOM_VHID_IOCTL_XUSB_DEACTIVATE 0x0022E444 /* Xbox 360; no buffers */
#define COROOM_VHID_IOCTL_XUSB_STATE 0x0022E448      /* Xbox 360; in: COROOM_VHID_XUSB_STATE */

/* COROOM_VHID_INFO.Kind */
#define COROOM_VHID_KIND_CONTROL 0
#define COROOM_VHID_KIND_XUSB 1

/* COROOM_VHID_OUTPUT.Type */
#define COROOM_VHID_OUTPUT_REPORT 1
#define COROOM_VHID_OUTPUT_FEATURE 2

/* Sizes of the structures below (checked with C_ASSERT here and by the Rust tests). */
#define COROOM_VHID_INFO_SIZE 8
#define COROOM_VHID_CREATE_SIZE 16
#define COROOM_VHID_INPUT_SIZE 4
#define COROOM_VHID_OUTPUT_HEADER_SIZE 8
#define COROOM_VHID_XUSB_ID_SIZE 4
#define COROOM_VHID_XUSB_STATE_SIZE 12

/* The Xbox 360 controller that converted gamepads present (wired, Microsoft). */
#define COROOM_VHID_XBOX360_VENDOR_ID 0x045E
#define COROOM_VHID_XBOX360_PRODUCT_ID 0x028E

#pragma pack(push, 1)

/* COROOM_VHID_IOCTL_GET_INFO. */
typedef struct _COROOM_VHID_INFO {
    UINT32 ProtocolVersion; /* COROOM_VHID_PROTOCOL_VERSION */
    UINT8 Kind;             /* COROOM_VHID_KIND_* */
    UINT8 Slot;             /* Xbox 360: its slot; control: COROOM_VHID_MAX_PADS */
    UINT8 Active;           /* Xbox 360: activated by a client; control: gamepads alive */
    UINT8 Reserved;         /* zero */
} COROOM_VHID_INFO, *PCOROOM_VHID_INFO;

/*
 * COROOM_VHID_IOCTL_CREATE: this header, then DescriptorLength bytes of report descriptor,
 * NeutralLength bytes of rest input report (0: no watchdog) and FeaturesLength bytes of feature
 * reports, each one as a UINT16 length followed by the report (its first byte is the report ID),
 * that the driver answers when Windows reads them (IOCTL_HID_GET_FEATURE). Creating a pad that
 * exists replaces it.
 */
typedef struct _COROOM_VHID_CREATE {
    UINT8 Pad;              /* 0 .. COROOM_VHID_MAX_PADS - 1 */
    UINT8 Reserved;         /* zero */
    UINT16 VendorId;
    UINT16 ProductId;
    UINT16 VersionNumber;
    UINT16 DescriptorLength; /* 1 .. COROOM_VHID_MAX_DESCRIPTOR */
    UINT16 NeutralLength;    /* 0 .. COROOM_VHID_MAX_REPORT */
    UINT16 FeaturesLength;   /* 0 .. COROOM_VHID_MAX_FEATURE_BYTES + 2 * COROOM_VHID_MAX_FEATURES */
    UINT16 Reserved2;        /* zero */
} COROOM_VHID_CREATE, *PCOROOM_VHID_CREATE;

/*
 * COROOM_VHID_IOCTL_INPUT: this header, then Length bytes of input report, exactly as the
 * descriptor declares it (its first byte is the report ID when the descriptor numbers them).
 */
typedef struct _COROOM_VHID_INPUT {
    UINT8 Pad;
    UINT8 Reserved;  /* zero */
    UINT16 Length;   /* 1 .. COROOM_VHID_MAX_REPORT */
} COROOM_VHID_INPUT, *PCOROOM_VHID_INPUT;

/*
 * COROOM_VHID_IOCTL_TAKE_OUTPUT: the oldest output the system sent to a gamepad, without waiting:
 * this header and Length bytes as Windows passed them (the client adds the report ID when it is
 * missing). Nothing returned (zero bytes) when there is none. The output buffer must hold
 * COROOM_VHID_OUTPUT_HEADER_SIZE + COROOM_VHID_MAX_REPORT bytes.
 */
typedef struct _COROOM_VHID_OUTPUT {
    UINT8 Pad;
    UINT8 Type;      /* COROOM_VHID_OUTPUT_* */
    UINT8 ReportId;  /* HID_XFER_PACKET.reportId */
    UINT8 Reserved;  /* zero */
    UINT16 Length;   /* 0 .. COROOM_VHID_MAX_REPORT */
    UINT16 Reserved2;/* zero */
} COROOM_VHID_OUTPUT, *PCOROOM_VHID_OUTPUT;

/* COROOM_VHID_IOCTL_XUSB_ACTIVATE: the identity the Xbox 360 device reports to XInput. */
typedef struct _COROOM_VHID_XUSB_ID {
    UINT16 VendorId;
    UINT16 ProductId;
} COROOM_VHID_XUSB_ID, *PCOROOM_VHID_XUSB_ID;

/* COROOM_VHID_IOCTL_XUSB_STATE: XINPUT_GAMEPAD. */
typedef struct _COROOM_VHID_XUSB_STATE {
    UINT16 Buttons;      /* XINPUT_GAMEPAD_* bits */
    UINT8 LeftTrigger;
    UINT8 RightTrigger;
    INT16 ThumbLX;
    INT16 ThumbLY;       /* up is positive */
    INT16 ThumbRX;
    INT16 ThumbRY;       /* up is positive */
} COROOM_VHID_XUSB_STATE, *PCOROOM_VHID_XUSB_STATE;

#pragma pack(pop)

/* C_ASSERT comes from the WDK and the Windows SDK (winnt.h). */
C_ASSERT(sizeof(COROOM_VHID_INFO) == COROOM_VHID_INFO_SIZE);
C_ASSERT(sizeof(COROOM_VHID_CREATE) == COROOM_VHID_CREATE_SIZE);
C_ASSERT(sizeof(COROOM_VHID_INPUT) == COROOM_VHID_INPUT_SIZE);
C_ASSERT(sizeof(COROOM_VHID_OUTPUT) == COROOM_VHID_OUTPUT_HEADER_SIZE);
C_ASSERT(sizeof(COROOM_VHID_XUSB_ID) == COROOM_VHID_XUSB_ID_SIZE);
C_ASSERT(sizeof(COROOM_VHID_XUSB_STATE) == COROOM_VHID_XUSB_STATE_SIZE);

#define COROOM_VHID_CTL(Function) \
    CTL_CODE(FILE_DEVICE_UNKNOWN, (Function), METHOD_BUFFERED, FILE_READ_DATA | FILE_WRITE_DATA)
C_ASSERT(COROOM_VHID_IOCTL_GET_INFO == COROOM_VHID_CTL(0x900));
C_ASSERT(COROOM_VHID_IOCTL_CREATE == COROOM_VHID_CTL(0x901));
C_ASSERT(COROOM_VHID_IOCTL_DESTROY == COROOM_VHID_CTL(0x902));
C_ASSERT(COROOM_VHID_IOCTL_INPUT == COROOM_VHID_CTL(0x903));
C_ASSERT(COROOM_VHID_IOCTL_TAKE_OUTPUT == COROOM_VHID_CTL(0x904));
C_ASSERT(COROOM_VHID_IOCTL_XUSB_ACTIVATE == COROOM_VHID_CTL(0x910));
C_ASSERT(COROOM_VHID_IOCTL_XUSB_DEACTIVATE == COROOM_VHID_CTL(0x911));
C_ASSERT(COROOM_VHID_IOCTL_XUSB_STATE == COROOM_VHID_CTL(0x912));

#endif /* COROOM_VHID_H */
