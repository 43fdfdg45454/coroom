/*
 * driver.h - internal declarations of coroomvhid.dll (UMDF2 HID minidriver).
 *
 * Device stack of every slot (one root device Root\coroomvhid per slot, see coroomvhid.inf):
 *
 *   hidclass.sys + mshidumdf.sys   function driver (inbox): the HID class driver and the
 *                                  pass-through minidriver that turns the HID IOCTLs into
 *                                  IOCTL_HID_* / IOCTL_UMDF_HID_* requests for user mode
 *   WUDFRd.sys                     reflector (inbox, lower filter)
 *   coroomvhid.dll (WUDFHost.exe)  this driver: a filter in UMDF terms (WdfFdoInitSetFilter), it
 *                                  answers the HID requests with the descriptors of coroom_vhid.h
 *
 * State of a device: the current gamepad input report (Report), whether hidclass still has to
 * get it (Pending) and the time of the last state report from the client (for the watchdog). All
 * of it is protected by Lock. Requests are never completed with Lock held: the report is copied
 * out under the lock and the request completed after releasing it.
 */

#ifndef COROOMVHID_DRIVER_H
#define COROOMVHID_DRIVER_H

/* The NTSTATUS values come from ntstatus.h, not from the reduced set in winnt.h. */
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <string.h> /* memcpy, memset and memcmp behind RtlCopyMemory and friends */

#include <wdf.h>
#include <hidport.h> /* WDK (km): HID_DESCRIPTOR, HID_XFER_PACKET, IOCTL_UMDF_HID_* */

#include "coroom_vhid.h"

/* Period of the watchdog timer, well below COROOM_VHID_WATCHDOG_MS. */
#define COROOM_WATCHDOG_PERIOD_MS 250

typedef struct _COROOM_DEVICE_CONTEXT {
    WDFDEVICE Device;
    WDFQUEUE ReadQueue;  /* manual queue: IOCTL_HID_READ_REPORT waiting for a new state */
    WDFTIMER Watchdog;   /* periodic, started in D0Entry and stopped in D0Exit */
    WDFWAITLOCK Lock;
    UCHAR Slot;          /* from COROOM_VHID_SLOT_VALUE, 0 .. COROOM_VHID_MAX_SLOTS - 1 */
    HID_DEVICE_ATTRIBUTES Attributes;
    HID_DESCRIPTOR HidDescriptor;
    WCHAR Serial[16];    /* "coroom-pad-N", N = Slot + 1 */
    /* Protected by Lock. */
    UCHAR Report[COROOM_VHID_REPORT_SIZE]; /* current state, report ID included */
    BOOLEAN Pending;     /* Report changed and no read request has carried it yet */
    BOOLEAN ClientActive;/* a state report arrived less than COROOM_VHID_WATCHDOG_MS ago */
    ULONGLONG LastStateTick; /* GetTickCount64() of the last state report */
} COROOM_DEVICE_CONTEXT, *PCOROOM_DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(COROOM_DEVICE_CONTEXT, CoroomGetDeviceContext)

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD CoroomEvtDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY CoroomEvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT CoroomEvtDeviceD0Exit;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL CoroomEvtIoDeviceControl;
EVT_WDF_TIMER CoroomEvtWatchdog;

#endif /* COROOMVHID_DRIVER_H */
