/*
 * coroomvhid.c - coroom virtual gamepad driver (KMDF + Virtual HID Framework).
 *
 * One root device (Root\coroomvhid, vhf.sys as lower filter) and one control device
 * (\\.\coroomvhid) through which coroom creates up to COROOM_VHID_MAX_SLOTS virtual HID gamepads
 * and feeds them input reports. The interface is in coroom_vhid.h, the state machine of the slots
 * in driver.h and the HID report descriptor in report_descriptor.inc.
 *
 * Rules kept throughout this file:
 *   - every IOCTL buffer size is checked before the buffer is touched, and the input is copied
 *     to the stack before it is validated;
 *   - VhfCreate, VhfStart and VhfDelete (always with Wait = TRUE, at PASSIVE_LEVEL) run without
 *     the driver lock held;
 *   - the report path (IOCTL_COROOM_VHID_REPORT) allocates nothing: each slot has its own
 *     preallocated HID_XFER_PACKET and report buffer, and VHF buffers the reports (no
 *     EvtVhfReadyForNextReadReport), so the buffer can be reused as soon as VhfReadReportSubmit
 *     returns.
 */

#include "driver.h"

#ifdef ALLOC_PRAGMA
#pragma alloc_text(INIT, DriverEntry)
#pragma alloc_text(PAGE, CoroomEvtDeviceAdd)
#pragma alloc_text(PAGE, CoroomCreateControlDevice)
#endif

/* The descriptor shared with the Rust client (see report_descriptor.inc). */
static const UCHAR CoroomReportDescriptor[] = {
#include "report_descriptor.inc"
};

C_ASSERT(sizeof(CoroomReportDescriptor) <= MAXUSHORT);
C_ASSERT(COROOM_VHID_MAX_SLOTS <= MAXUCHAR);
C_ASSERT(COROOM_VHID_REPORT_OFFSET_HAT < COROOM_VHID_REPORT_SIZE);

/* ------------------------------------------------------------------------------------------- */
/* Driver and root device                                                                      */
/* ------------------------------------------------------------------------------------------- */

NTSTATUS
DriverEntry(
    _In_ PDRIVER_OBJECT DriverObject,
    _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES lockAttributes;
    WDFDRIVER driver;
    PCOROOM_DRIVER_CONTEXT context;
    NTSTATUS status;
    ULONG i;

    WDF_DRIVER_CONFIG_INIT(&config, CoroomEvtDeviceAdd);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, COROOM_DRIVER_CONTEXT);

    status = WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, &driver);
    if (!NT_SUCCESS(status)) {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: WdfDriverCreate failed 0x%08X\n", status));
        return status;
    }

    /* The framework zeroes the context: every slot starts Free, without owner or VHF handle. */
    context = CoroomGetDriverContext(driver);
    KeInitializeEvent(&context->IdleEvent, NotificationEvent, TRUE);
    for (i = 0; i < COROOM_VHID_MAX_SLOTS; i++) {
        context->Slots[i].Packet.reportBuffer = context->Slots[i].Report;
        context->Slots[i].Packet.reportBufferLen = COROOM_VHID_REPORT_SIZE;
        context->Slots[i].Packet.reportId = COROOM_VHID_REPORT_ID;
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
    lockAttributes.ParentObject = driver;
    status = WdfWaitLockCreate(&lockAttributes, &context->Lock);
    if (!NT_SUCCESS(status)) {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: WdfWaitLockCreate failed 0x%08X\n", status));
    }
    return status;
}

NTSTATUS
CoroomEvtDeviceAdd(
    _In_ WDFDRIVER Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    PCOROOM_DRIVER_CONTEXT context;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDFDEVICE fdo = NULL;
    WDFDEVICE control = NULL;
    NTSTATUS status;
    BOOLEAN taken;

    PAGED_CODE();

    context = CoroomGetDriverContext(Driver);

    /* A single root device: its control device has a fixed name. */
    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    taken = (context->Fdo != NULL) || (context->ControlDevice != NULL) || context->Adding;
    if (!taken) {
        context->Adding = TRUE;
    }
    WdfWaitLockRelease(context->Lock);
    if (taken) {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: only one Root\\coroomvhid device is supported\n"));
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    WdfDeviceInitSetDeviceType(DeviceInit, FILE_DEVICE_UNKNOWN);
    WdfDeviceInitSetCharacteristics(DeviceInit, FILE_DEVICE_SECURE_OPEN, TRUE);

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.EvtCleanupCallback = CoroomEvtFdoCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attributes, &fdo);
    if (NT_SUCCESS(status)) {
        status = CoroomCreateControlDevice(Driver, &control);
    } else {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: WdfDeviceCreate (root) failed 0x%08X\n", status));
    }

    /*
     * On failure the framework deletes the FDO after this returns; CoroomEvtFdoCleanup ignores it
     * because it was never published in the context.
     */
    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    context->Adding = FALSE;
    if (NT_SUCCESS(status)) {
        context->Fdo = fdo;
        context->ControlDevice = control;
        context->FdoRemoving = FALSE;
    }
    WdfWaitLockRelease(context->Lock);
    return status;
}

VOID
CoroomEvtFdoCleanup(
    _In_ WDFOBJECT Object)
{
    PCOROOM_DRIVER_CONTEXT context;
    WDFDEVICE control;

    context = CoroomGetDriverContext(WdfGetDriver());

    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    if (context->Fdo != (WDFDEVICE)Object) {
        WdfWaitLockRelease(context->Lock);
        return;
    }
    context->FdoRemoving = TRUE;
    WdfWaitLockRelease(context->Lock);

    /* No slot can be created any more: delete the active ones and wait for those in transit. */
    CoroomDestroySlots(context, NULL);
    CoroomWaitIdle(context);

    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    control = context->ControlDevice;
    context->ControlDevice = NULL;
    context->Fdo = NULL;
    WdfWaitLockRelease(context->Lock);

    /* Otherwise the driver could never unload. Open handles only get errors from now on. */
    if (control != NULL) {
        WdfObjectDelete(control);
    }
}

/* ------------------------------------------------------------------------------------------- */
/* Control device                                                                              */
/* ------------------------------------------------------------------------------------------- */

_Use_decl_annotations_
NTSTATUS
CoroomCreateControlDevice(
    WDFDRIVER Driver,
    WDFDEVICE *ControlDevice)
{
    DECLARE_CONST_UNICODE_STRING(sddl, COROOM_VHID_SDDL);
    DECLARE_CONST_UNICODE_STRING(ntName, COROOM_VHID_NT_DEVICE_NAME);
    DECLARE_CONST_UNICODE_STRING(dosName, COROOM_VHID_DOS_DEVICE_NAME);
    PWDFDEVICE_INIT init;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_OBJECT_ATTRIBUTES fileAttributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_OBJECT_ATTRIBUTES queueAttributes;
    WDFDEVICE device = NULL;
    NTSTATUS status;

    PAGED_CODE();

    *ControlDevice = NULL;

    init = WdfControlDeviceInitAllocate(Driver, &sddl);
    if (init == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    WdfDeviceInitSetDeviceType(init, FILE_DEVICE_UNKNOWN);
    /* The SDDL also applies to names below the device (\\.\coroomvhid\anything). */
    WdfDeviceInitSetCharacteristics(init, FILE_DEVICE_SECURE_OPEN, TRUE);
    WdfDeviceInitSetExclusive(init, FALSE);
    WdfDeviceInitSetIoType(init, WdfDeviceIoBuffered);

    status = WdfDeviceInitAssignName(init, &ntName);
    if (!NT_SUCCESS(status)) {
        goto Exit;
    }

    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, WDF_NO_EVENT_CALLBACK, CoroomEvtFileClose,
                               CoroomEvtFileCleanup);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&fileAttributes, COROOM_FILE_CONTEXT);
    WdfDeviceInitSetFileObjectConfig(init, &fileConfig, &fileAttributes);

    status = WdfDeviceCreate(&init, WDF_NO_OBJECT_ATTRIBUTES, &device);
    if (!NT_SUCCESS(status)) {
        goto Exit;
    }

    /*
     * One request at a time, at PASSIVE_LEVEL (VhfCreate and VhfDelete need it). A control device
     * is not power managed.
     */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchSequential);
    queueConfig.EvtIoDeviceControl = CoroomEvtIoDeviceControl;
    queueConfig.PowerManaged = WdfFalse;
    WDF_OBJECT_ATTRIBUTES_INIT(&queueAttributes);
    queueAttributes.ExecutionLevel = WdfExecutionLevelPassive;
    status = WdfIoQueueCreate(device, &queueConfig, &queueAttributes, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        goto Exit;
    }

    status = WdfDeviceCreateSymbolicLink(device, &dosName);
    if (!NT_SUCCESS(status)) {
        goto Exit;
    }

    WdfControlFinishInitializing(device);
    *ControlDevice = device;
    device = NULL;

Exit:
    if (!NT_SUCCESS(status)) {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: control device creation failed 0x%08X\n", status));
    }
    if (device != NULL) {
        WdfObjectDelete(device);
    }
    /* WdfDeviceCreate sets init to NULL when it takes ownership of it. */
    if (init != NULL) {
        WdfDeviceInitFree(init);
    }
    return status;
}

VOID
CoroomEvtFileCleanup(
    _In_ WDFFILEOBJECT FileObject)
{
    PCOROOM_DRIVER_CONTEXT context = CoroomGetDriverContext(WdfGetDriver());

    /* The handle is closing (also when coroom dies): its gamepads disappear with it. */
    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    CoroomGetFileContext(FileObject)->Closing = TRUE;
    WdfWaitLockRelease(context->Lock);

    CoroomDestroySlots(context, FileObject);
}

VOID
CoroomEvtFileClose(
    _In_ WDFFILEOBJECT FileObject)
{
    /*
     * Normally there is nothing left: EvtFileCleanup already destroyed everything and a CREATE
     * that was in flight during it undoes itself (Closing). This is only a safety net.
     */
    CoroomDestroySlots(CoroomGetDriverContext(WdfGetDriver()), FileObject);
}

/* ------------------------------------------------------------------------------------------- */
/* Slots                                                                                       */
/* ------------------------------------------------------------------------------------------- */

_Requires_lock_held_(Context->Lock)
static VOID
CoroomBusyEnter(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context)
{
    if (Context->BusyCount++ == 0) {
        KeClearEvent(&Context->IdleEvent);
    }
}

_Requires_lock_held_(Context->Lock)
static VOID
CoroomBusyLeave(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context)
{
    NT_ASSERT(Context->BusyCount > 0);
    if (--Context->BusyCount == 0) {
        KeSetEvent(&Context->IdleEvent, IO_NO_INCREMENT, FALSE);
    }
}

_Requires_lock_held_(Context->Lock)
static VOID
CoroomSlotSetFree(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context,
    _Inout_ PCOROOM_SLOT Slot)
{
    Slot->Vhf = NULL;
    Slot->Owner = NULL;
    Slot->State = CoroomSlotFree;
    RtlZeroMemory(Slot->Report, sizeof(Slot->Report));
    CoroomBusyLeave(Context);
}

_Use_decl_annotations_
VOID
CoroomDestroySlots(
    PCOROOM_DRIVER_CONTEXT Context,
    WDFFILEOBJECT Owner)
{
    VHFHANDLE handles[COROOM_VHID_MAX_SLOTS];
    ULONG indices[COROOM_VHID_MAX_SLOTS];
    ULONG count = 0;
    ULONG i;

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    for (i = 0; i < COROOM_VHID_MAX_SLOTS; i++) {
        PCOROOM_SLOT slot = &Context->Slots[i];
        if (slot->State == CoroomSlotActive && (Owner == NULL || slot->Owner == Owner)) {
            handles[count] = slot->Vhf;
            indices[count] = i;
            count++;
            slot->Vhf = NULL;
            slot->State = CoroomSlotDeleting;
            CoroomBusyEnter(Context);
        }
    }
    WdfWaitLockRelease(Context->Lock);

    if (count == 0) {
        return;
    }

    for (i = 0; i < count; i++) {
        VhfDelete(handles[i], TRUE);
    }

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    for (i = 0; i < count; i++) {
        CoroomSlotSetFree(Context, &Context->Slots[indices[i]]);
    }
    WdfWaitLockRelease(Context->Lock);
}

_Use_decl_annotations_
VOID
CoroomWaitIdle(
    PCOROOM_DRIVER_CONTEXT Context)
{
    /* The event mirrors BusyCount == 0 (both change under the lock). */
    (VOID)KeWaitForSingleObject(&Context->IdleEvent, Executive, KernelMode, FALSE, NULL);
}

/* ------------------------------------------------------------------------------------------- */
/* IOCTLs                                                                                      */
/* ------------------------------------------------------------------------------------------- */

_IRQL_requires_max_(PASSIVE_LEVEL)
static NTSTATUS
CoroomReadSlotIndex(
    _In_ WDFREQUEST Request,
    _In_ size_t InputBufferLength,
    _In_ size_t OutputBufferLength,
    _Out_ ULONG *Index)
{
    COROOM_VHID_SLOT input;
    PVOID buffer;
    NTSTATUS status;

    *Index = 0;
    if (InputBufferLength != sizeof(COROOM_VHID_SLOT) || OutputBufferLength != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(COROOM_VHID_SLOT), &buffer, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlCopyMemory(&input, buffer, sizeof(input));
    if (input.Slot >= COROOM_VHID_MAX_SLOTS) {
        return STATUS_INVALID_PARAMETER;
    }
    *Index = input.Slot;
    return STATUS_SUCCESS;
}

static INT16
CoroomReadInt16(
    _In_reads_bytes_(COROOM_VHID_REPORT_SIZE) const UCHAR *Report,
    _In_range_(0, COROOM_VHID_REPORT_SIZE - 2) ULONG Offset)
{
    return (INT16)(USHORT)((USHORT)Report[Offset] | (USHORT)((USHORT)Report[Offset + 1] << 8));
}

/* The values of the report must be inside the logical ranges of the descriptor. */
static BOOLEAN
CoroomReportIsValid(
    _In_reads_bytes_(COROOM_VHID_REPORT_SIZE) const UCHAR *Report)
{
    static const ULONG sticks[] = {
        COROOM_VHID_REPORT_OFFSET_X,
        COROOM_VHID_REPORT_OFFSET_Y,
        COROOM_VHID_REPORT_OFFSET_RX,
        COROOM_VHID_REPORT_OFFSET_RY,
    };
    static const ULONG triggers[] = {
        COROOM_VHID_REPORT_OFFSET_Z,
        COROOM_VHID_REPORT_OFFSET_RZ,
    };
    ULONG i;
    UCHAR hat;

    if (Report[0] != COROOM_VHID_REPORT_ID) {
        return FALSE;
    }
    for (i = 0; i < RTL_NUMBER_OF(sticks); i++) {
        if (CoroomReadInt16(Report, sticks[i]) < -COROOM_VHID_AXIS_MAX) {
            return FALSE;
        }
    }
    for (i = 0; i < RTL_NUMBER_OF(triggers); i++) {
        if (CoroomReadInt16(Report, triggers[i]) < 0) {
            return FALSE;
        }
    }
    hat = Report[COROOM_VHID_REPORT_OFFSET_HAT];
    if ((hat & 0x0F) > COROOM_VHID_HAT_NULL || (hat & 0xF0) != 0) {
        return FALSE;
    }
    return TRUE;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static NTSTATUS
CoroomIoctlVersion(
    _In_ WDFREQUEST Request,
    _In_ size_t InputBufferLength,
    _In_ size_t OutputBufferLength,
    _Out_ ULONG_PTR *Information)
{
    PCOROOM_VHID_VERSION_INFO info;
    PVOID buffer;
    NTSTATUS status;

    *Information = 0;
    /* A larger output buffer is accepted so that newer clients can still read the version. */
    if (InputBufferLength != 0 || OutputBufferLength < sizeof(COROOM_VHID_VERSION_INFO)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    status = WdfRequestRetrieveOutputBuffer(Request, sizeof(COROOM_VHID_VERSION_INFO), &buffer,
                                            NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    info = (PCOROOM_VHID_VERSION_INFO)buffer;
    RtlZeroMemory(info, sizeof(*info));
    info->ProtocolVersion = COROOM_VHID_PROTOCOL_VERSION;
    info->MaxSlots = COROOM_VHID_MAX_SLOTS;
    info->ReportSize = COROOM_VHID_REPORT_SIZE;
    *Information = sizeof(*info);
    return STATUS_SUCCESS;
}

_IRQL_requires_(PASSIVE_LEVEL)
static NTSTATUS
CoroomIoctlCreate(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context,
    _In_ WDFFILEOBJECT File,
    _In_ WDFREQUEST Request,
    _In_ size_t InputBufferLength,
    _In_ size_t OutputBufferLength)
{
    PCOROOM_SLOT slot;
    PDEVICE_OBJECT parent = NULL;
    VHF_CONFIG config;
    VHFHANDLE vhf = NULL;
    NTSTATUS status;
    ULONG index;
    BOOLEAN claimed = FALSE;

    status = CoroomReadSlotIndex(Request, InputBufferLength, OutputBufferLength, &index);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    slot = &Context->Slots[index];

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (Context->Fdo == NULL || Context->FdoRemoving) {
        status = STATUS_DEVICE_REMOVED;
    } else if (CoroomGetFileContext(File)->Closing) {
        status = STATUS_FILE_CLOSED;
    } else if (slot->State == CoroomSlotActive && slot->Owner == File) {
        status = STATUS_SUCCESS; /* already created by this handle */
    } else if (slot->State != CoroomSlotFree) {
        status = STATUS_DEVICE_BUSY;
    } else {
        slot->State = CoroomSlotCreating;
        slot->Owner = File;
        CoroomBusyEnter(Context);
        /* Valid until BusyCount drops: CoroomEvtFdoCleanup waits for it. */
        parent = WdfDeviceWdmGetDeviceObject(Context->Fdo);
        claimed = TRUE;
    }
    WdfWaitLockRelease(Context->Lock);
    if (!claimed) {
        return status;
    }

    VHF_CONFIG_INIT(&config, parent, (USHORT)sizeof(CoroomReportDescriptor),
                    (PUCHAR)CoroomReportDescriptor);
    config.VendorID = COROOM_VHID_VENDOR_ID;
    config.ProductID = COROOM_VHID_PRODUCT_ID;
    config.VersionNumber = COROOM_VHID_VERSION_NUMBER;

    status = VhfCreate(&config, &vhf);
    if (NT_SUCCESS(status)) {
        status = VhfStart(vhf);
        if (!NT_SUCCESS(status)) {
            KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                       "coroomvhid: VhfStart (slot %lu) failed 0x%08X\n", index, status));
            VhfDelete(vhf, TRUE);
            vhf = NULL;
        }
    } else {
        KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL,
                   "coroomvhid: VhfCreate (slot %lu) failed 0x%08X\n", index, status));
        vhf = NULL;
    }

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (NT_SUCCESS(status)) {
        if (!Context->FdoRemoving && !CoroomGetFileContext(File)->Closing) {
            slot->Vhf = vhf;
            slot->State = CoroomSlotActive;
            CoroomBusyLeave(Context);
            WdfWaitLockRelease(Context->Lock);
            return STATUS_SUCCESS;
        }
        /* The handle or the root device went away meanwhile: undo. */
        status = Context->FdoRemoving ? STATUS_DEVICE_REMOVED : STATUS_FILE_CLOSED;
        slot->State = CoroomSlotDeleting;
    }
    WdfWaitLockRelease(Context->Lock);

    if (vhf != NULL) {
        VhfDelete(vhf, TRUE);
    }

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    CoroomSlotSetFree(Context, slot);
    WdfWaitLockRelease(Context->Lock);
    return status;
}

_IRQL_requires_(PASSIVE_LEVEL)
static NTSTATUS
CoroomIoctlDestroy(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context,
    _In_ WDFFILEOBJECT File,
    _In_ WDFREQUEST Request,
    _In_ size_t InputBufferLength,
    _In_ size_t OutputBufferLength)
{
    PCOROOM_SLOT slot;
    VHFHANDLE vhf = NULL;
    NTSTATUS status;
    ULONG index;

    status = CoroomReadSlotIndex(Request, InputBufferLength, OutputBufferLength, &index);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    slot = &Context->Slots[index];

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (slot->State == CoroomSlotFree) {
        status = STATUS_INVALID_DEVICE_STATE;
    } else if (slot->Owner != File) {
        status = STATUS_ACCESS_DENIED;
    } else if (slot->State != CoroomSlotActive) {
        status = STATUS_DEVICE_BUSY;
    } else {
        vhf = slot->Vhf;
        slot->Vhf = NULL;
        slot->State = CoroomSlotDeleting;
        CoroomBusyEnter(Context);
    }
    WdfWaitLockRelease(Context->Lock);
    if (vhf == NULL) {
        return status;
    }

    VhfDelete(vhf, TRUE);

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    CoroomSlotSetFree(Context, slot);
    WdfWaitLockRelease(Context->Lock);
    return STATUS_SUCCESS;
}

_IRQL_requires_(PASSIVE_LEVEL)
static NTSTATUS
CoroomIoctlReport(
    _Inout_ PCOROOM_DRIVER_CONTEXT Context,
    _In_ WDFFILEOBJECT File,
    _In_ WDFREQUEST Request,
    _In_ size_t InputBufferLength,
    _In_ size_t OutputBufferLength)
{
    COROOM_VHID_REPORT_PACKET packet;
    PCOROOM_SLOT slot;
    PVOID buffer;
    NTSTATUS status;

    if (InputBufferLength != sizeof(COROOM_VHID_REPORT_PACKET) || OutputBufferLength != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(COROOM_VHID_REPORT_PACKET), &buffer,
                                           NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlCopyMemory(&packet, buffer, sizeof(packet));
    if (packet.Slot >= COROOM_VHID_MAX_SLOTS || !CoroomReportIsValid(packet.Report)) {
        return STATUS_INVALID_PARAMETER;
    }
    slot = &Context->Slots[packet.Slot];

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (slot->State == CoroomSlotActive && slot->Owner == File) {
        RtlCopyMemory(slot->Report, packet.Report, COROOM_VHID_REPORT_SIZE);
        status = VhfReadReportSubmit(slot->Vhf, &slot->Packet);
    } else if (slot->State != CoroomSlotFree && slot->Owner != File) {
        status = STATUS_ACCESS_DENIED;
    } else {
        status = STATUS_INVALID_DEVICE_STATE;
    }
    WdfWaitLockRelease(Context->Lock);
    return status;
}

VOID
CoroomEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    PCOROOM_DRIVER_CONTEXT context = CoroomGetDriverContext(WdfGetDriver());
    WDFFILEOBJECT file = WdfRequestGetFileObject(Request);
    ULONG_PTR information = 0;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Queue);

    if (file == NULL) {
        status = STATUS_INVALID_DEVICE_REQUEST;
    } else {
        switch (IoControlCode) {
        case IOCTL_COROOM_VHID_VERSION:
            status = CoroomIoctlVersion(Request, InputBufferLength, OutputBufferLength,
                                        &information);
            break;
        case IOCTL_COROOM_VHID_CREATE:
            status = CoroomIoctlCreate(context, file, Request, InputBufferLength,
                                       OutputBufferLength);
            break;
        case IOCTL_COROOM_VHID_DESTROY:
            status = CoroomIoctlDestroy(context, file, Request, InputBufferLength,
                                        OutputBufferLength);
            break;
        case IOCTL_COROOM_VHID_REPORT:
            status = CoroomIoctlReport(context, file, Request, InputBufferLength,
                                       OutputBufferLength);
            break;
        default:
            status = STATUS_INVALID_DEVICE_REQUEST;
            break;
        }
    }

    WdfRequestCompleteWithInformation(Request, status, information);
}
