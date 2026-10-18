/*
 * driver.h - internal declarations of coroomvhid.dll (UMDF2, protocol 4).
 *
 * Device stacks (see coroomvhid.inf):
 *
 *   Root\coroomvhid   WUDFRd.sys (function driver) + coroomvhid.dll in WUDFHost.exe, over vhf.sys
 *                     (lower filter). The DLL is the HID source driver: every gamepad is a VHF
 *                     device (VhfCreate) with its own WDFIOTARGET opened on the stack below
 *                     (WdfIoTargetOpenLocalTargetByFile), the user-mode way of giving VHF a file.
 *   Root\coroomxusb   WUDFRd.sys + coroomvhid.dll: an Xbox 360 controller (xusb22 IOCTLs).
 *
 * The kind of device comes from the CoroomKind value of its hardware key (the INF writes it).
 *
 * Locks: every device has one WDFWAITLOCK for its state. VHF calls that wait (VhfDelete) and
 * request completions are never made with it held; VHF callbacks take it only to copy data.
 */

#ifndef COROOMVHID_DRIVER_H
#define COROOMVHID_DRIVER_H

/* The NTSTATUS values come from ntstatus.h, not from the reduced set in winnt.h. */
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <winioctl.h> /* CTL_CODE */
#include <initguid.h> /* DEFINE_GUID defines the GUIDs below in this DLL */
#include <stdlib.h>   /* malloc, free */
#include <string.h>   /* memcpy, memset and memcmp behind RtlCopyMemory and friends */

#include <wdf.h>
#include <hidclass.h> /* HID_XFER_PACKET */
#include <vhf.h>     /* WDK: Virtual HID Framework (VhfUm.lib in user mode) */

#include "coroom_vhid.h"

/* COROOM_VHID_INTERFACE_GUID of coroom_vhid.h. */
DEFINE_GUID(GUID_DEVINTERFACE_COROOM_VHID,
    0x7c6b2c3e, 0x55d1, 0x4c7a, 0x9c, 0x4b, 0x2e, 0x0b, 0x6f, 0x1d, 0x8a, 0x41);

/* xusb22.sys: the device interface of Xbox 360 controllers that xinput1_4.dll enumerates. */
DEFINE_GUID(GUID_DEVINTERFACE_COROOM_XUSB,
    0xec87f1e3, 0xc13b, 0x4100, 0xb5, 0xf7, 0x8b, 0x84, 0xd5, 0x42, 0x60, 0xcb);

/* Value of the hardware key with the kind of device (COROOM_VHID_KIND_*). */
#define COROOM_KIND_VALUE L"CoroomKind"

/* Period of the watchdog timers, well below COROOM_VHID_WATCHDOG_MS. */
#define COROOM_WATCHDOG_PERIOD_MS 250

/* Period of the WAIT_FOR_INPUT pump of the Xbox 360 devices (a wired controller polls at 125 Hz). */
#define COROOM_XUSB_PUMP_MS 8

/* ------------------------------------------------------------------------------------------- */
/* Control device                                                                              */
/* ------------------------------------------------------------------------------------------- */

struct _COROOM_CONTROL_CONTEXT;

/* One gamepad (a VHF device). Allocated buffers belong to it until it is destroyed. */
typedef struct _COROOM_PAD {
    struct _COROOM_CONTROL_CONTEXT *Control;
    UCHAR Number;              /* index in COROOM_CONTROL_CONTEXT.Pads */
    /* Set and cleared by the I/O queue only (sequential). */
    VHFHANDLE Vhf;
    WDFIOTARGET Target;
    UCHAR *Descriptor;
    USHORT DescriptorLength;
    BOOLEAN Numbered;          /* the descriptor uses report IDs */
    /* Protected by the control lock (the VHF callbacks read them). */
    BOOLEAN Live;              /* created and not being destroyed */
    UCHAR *Features;           /* COROOM_VHID_CREATE feature blob (UINT16 length + report) */
    USHORT FeaturesLength;
    UCHAR Neutral[COROOM_VHID_MAX_REPORT];
    USHORT NeutralLength;      /* 0: no watchdog */
    UCHAR LastInput[COROOM_VHID_MAX_REPORT];
    USHORT LastInputLength;
    ULONGLONG LastInputTick;
    BOOLEAN Resting;           /* the watchdog put it at rest */
} COROOM_PAD, *PCOROOM_PAD;

typedef struct _COROOM_OUTPUT_ENTRY {
    COROOM_VHID_OUTPUT Header;
    UCHAR Data[COROOM_VHID_MAX_REPORT];
} COROOM_OUTPUT_ENTRY;

typedef struct _COROOM_CONTROL_CONTEXT {
    WDFDEVICE Device;
    WDFWAITLOCK Lock;
    WDFTIMER Watchdog;
    COROOM_PAD Pads[COROOM_VHID_MAX_PADS];
    /* Protected by Lock: ring of outputs for COROOM_VHID_IOCTL_TAKE_OUTPUT. */
    COROOM_OUTPUT_ENTRY Outputs[COROOM_VHID_MAX_OUTPUTS];
    ULONG OutputHead;          /* oldest */
    ULONG OutputCount;
} COROOM_CONTROL_CONTEXT, *PCOROOM_CONTROL_CONTEXT;

/* ------------------------------------------------------------------------------------------- */
/* Xbox 360 device                                                                             */
/* ------------------------------------------------------------------------------------------- */

typedef struct _COROOM_XUSB_CONTEXT {
    WDFDEVICE Device;
    WDFWAITLOCK Lock;
    WDFQUEUE WaitQueue;        /* manual: IOCTL_XUSB_WAIT_FOR_INPUT */
    WDFTIMER Pump;             /* completes one WAIT_FOR_INPUT per tick */
    WDFTIMER Watchdog;
    UCHAR Slot;                /* from COROOM_VHID_SLOT_VALUE */
    BOOLEAN Started;           /* SelfManagedIoInit ran: the interface state can change */
    /* Protected by Lock. */
    WDFFILEOBJECT Owner;       /* the client that activated it, or NULL */
    USHORT VendorId;
    USHORT ProductId;
    COROOM_VHID_XUSB_STATE State;
    ULONG PacketNumber;
    ULONGLONG LastStateTick;
    BOOLEAN Resting;
    /* Vibration games asked for (IOCTL_XUSB_SET_STATE), for COROOM_VHID_IOCTL_XUSB_RUMBLE. */
    COROOM_VHID_XUSB_RUMBLE Rumble;
} COROOM_XUSB_CONTEXT, *PCOROOM_XUSB_CONTEXT;

/* Both kinds share the device context type; Kind tells which half is in use. */
typedef struct _COROOM_DEVICE_CONTEXT {
    UCHAR Kind;                /* COROOM_VHID_KIND_* */
    PCOROOM_CONTROL_CONTEXT Control; /* allocated for the control device */
    PCOROOM_XUSB_CONTEXT Xusb;       /* allocated for an Xbox 360 device */
} COROOM_DEVICE_CONTEXT, *PCOROOM_DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(COROOM_DEVICE_CONTEXT, CoroomGetDeviceContext)

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD CoroomEvtDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP CoroomEvtDeviceCleanup;
EVT_WDF_DEVICE_D0_ENTRY CoroomEvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT CoroomEvtDeviceD0Exit;
EVT_WDF_DEVICE_SELF_MANAGED_IO_INIT CoroomEvtSelfManagedIoInit;
EVT_WDF_DEVICE_SELF_MANAGED_IO_CLEANUP CoroomEvtSelfManagedIoCleanup;
EVT_WDF_FILE_CLEANUP CoroomEvtFileCleanup;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL CoroomEvtControlIoctl;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL CoroomEvtXusbIoctl;
EVT_WDF_TIMER CoroomEvtControlWatchdog;
EVT_WDF_TIMER CoroomEvtXusbWatchdog;
EVT_WDF_TIMER CoroomEvtXusbPump;
EVT_VHF_ASYNC_OPERATION CoroomEvtVhfGetFeature;
EVT_VHF_ASYNC_OPERATION CoroomEvtVhfSetFeature;
EVT_VHF_ASYNC_OPERATION CoroomEvtVhfWriteReport;
EVT_VHF_ASYNC_OPERATION CoroomEvtVhfGetInputReport;

#endif /* COROOMVHID_DRIVER_H */
