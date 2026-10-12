/*
 * driver.h - internal declarations of coroomvhid.sys.
 *
 * Objects:
 *   - Root device (FDO): PnP device "Root\coroomvhid" installed by coroomvhid.inf, with vhf.sys as
 *     lower filter. It owns no I/O; its WDM device object is the parent of the virtual HID devices
 *     that VhfCreate enumerates.
 *   - Control device: non-PnP, \Device\coroomvhid with the symbolic link \DosDevices\coroomvhid,
 *     protected by COROOM_VHID_SDDL. It serves the IOCTLs of coroom_vhid.h on a sequential queue.
 *   - Slots: up to COROOM_VHID_MAX_SLOTS virtual gamepads, each one a VHF device created on
 *     demand and owned by the WDFFILEOBJECT (handle) that created it.
 *
 * All the shared state lives in the WDFDRIVER context and is protected by a wait lock. VhfCreate,
 * VhfStart and VhfDelete are never called with the lock held (VhfDelete waits for PnP): a slot
 * moves Free -> Creating -> Active -> Deleting -> Free, the transitions are made under the lock
 * and BusyCount counts the slots in Creating or Deleting, so that the removal of the root device
 * can wait for them before its WDM device object goes away.
 */

#ifndef COROOMVHID_DRIVER_H
#define COROOMVHID_DRIVER_H

#include <ntddk.h>
#include <wdf.h>
#include <hidclass.h>
#include <vhf.h>

#include "coroom_vhid.h"

typedef enum _COROOM_SLOT_STATE {
    CoroomSlotFree = 0,
    CoroomSlotCreating,
    CoroomSlotActive,
    CoroomSlotDeleting,
} COROOM_SLOT_STATE;

typedef struct _COROOM_SLOT {
    COROOM_SLOT_STATE State;
    VHFHANDLE Vhf;          /* valid in CoroomSlotActive (and while Deleting, owned by the deleter) */
    WDFFILEOBJECT Owner;    /* handle that created the slot; NULL when Free */
    HID_XFER_PACKET Packet; /* preallocated: points at Report, no allocation in the report path */
    UCHAR Report[COROOM_VHID_REPORT_SIZE];
} COROOM_SLOT, *PCOROOM_SLOT;

typedef struct _COROOM_DRIVER_CONTEXT {
    WDFWAITLOCK Lock;
    WDFDEVICE Fdo;           /* the root device, NULL when there is none */
    WDFDEVICE ControlDevice; /* created with the root device, deleted with it */
    BOOLEAN Adding;          /* EvtDriverDeviceAdd is creating the root and control devices */
    BOOLEAN FdoRemoving;     /* the root device is going away: no new slots */
    ULONG BusyCount;         /* slots in Creating or Deleting */
    KEVENT IdleEvent;        /* signalled when BusyCount drops to zero */
    COROOM_SLOT Slots[COROOM_VHID_MAX_SLOTS];
} COROOM_DRIVER_CONTEXT, *PCOROOM_DRIVER_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(COROOM_DRIVER_CONTEXT, CoroomGetDriverContext)

typedef struct _COROOM_FILE_CONTEXT {
    BOOLEAN Closing; /* EvtFileCleanup ran: the handle cannot create slots any more */
} COROOM_FILE_CONTEXT, *PCOROOM_FILE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(COROOM_FILE_CONTEXT, CoroomGetFileContext)

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD CoroomEvtDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP CoroomEvtFdoCleanup;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL CoroomEvtIoDeviceControl;
EVT_WDF_FILE_CLEANUP CoroomEvtFileCleanup;
EVT_WDF_FILE_CLOSE CoroomEvtFileClose;

_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_
NTSTATUS
CoroomCreateControlDevice(
    _In_ WDFDRIVER Driver,
    _Out_ WDFDEVICE *ControlDevice);

_IRQL_requires_(PASSIVE_LEVEL)
VOID
CoroomDestroySlots(
    _In_ PCOROOM_DRIVER_CONTEXT Context,
    _In_opt_ WDFFILEOBJECT Owner);

_IRQL_requires_(PASSIVE_LEVEL)
VOID
CoroomWaitIdle(
    _In_ PCOROOM_DRIVER_CONTEXT Context);

#endif /* COROOMVHID_DRIVER_H */
