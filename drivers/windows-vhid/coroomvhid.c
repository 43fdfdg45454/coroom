/*
 * coroomvhid.c - coroom virtual gamepad driver (UMDF2 HID minidriver under mshidumdf.sys).
 *
 * One instance per slot (root device Root\coroomvhid created by install.ps1). It answers the HID
 * requests that mshidumdf.sys forwards from hidclass.sys, the same way as Microsoft's vhidmini2
 * UMDF2 sample (hid/vhidmini2 in Windows-driver-samples), with the descriptor of
 * report_descriptor.inc (the gamepad, Col01) followed by control_descriptor.inc (Col02, where
 * coroom writes the state). The interface is in coroom_vhid.h and the device state in driver.h.
 *
 * Rules kept throughout this file:
 *   - every buffer size is checked before the buffer is touched, and the client data is copied to
 *     the stack before it is validated;
 *   - requests are completed without the device lock held;
 *   - a state equal to the current one (the client repeats it as a keepalive) only feeds the
 *     watchdog: no input report goes to hidclass, so games do not see duplicates.
 *
 * It runs in user mode (WUDFHost.exe): a bug here ends that host process, not the system, and the
 * binary needs no kernel-mode signature, only a driver package catalog trusted on the machine.
 */

#include "driver.h"

/* Gamepad (Col01) followed by the control collection (Col02); see the two .inc files. */
static const UCHAR CoroomReportDescriptor[] = {
#include "report_descriptor.inc"
#include "control_descriptor.inc"
};

C_ASSERT(sizeof(CoroomReportDescriptor) <= 0xFFFF);
C_ASSERT(COROOM_VHID_MAX_SLOTS <= 9); /* the serial number has one digit */
C_ASSERT(COROOM_VHID_REPORT_OFFSET_HAT < COROOM_VHID_REPORT_SIZE);

static const WCHAR CoroomManufacturer[] = COROOM_VHID_MANUFACTURER;
static const WCHAR CoroomProduct[] = COROOM_VHID_PRODUCT;

/* Serial number: this prefix and the slot number (1 to 8), so SDL tells the gamepads apart. */
static const WCHAR CoroomSerialPrefix[] = L"coroom-pad-";
#define COROOM_SERIAL_CHARS (ARRAYSIZE(CoroomSerialPrefix) + 1) /* prefix, digit, NUL */
C_ASSERT(COROOM_SERIAL_CHARS <= RTL_NUMBER_OF_FIELD(COROOM_DEVICE_CONTEXT, Serial));

/* ------------------------------------------------------------------------------------------- */
/* Request buffers (as util.c of the vhidmini2 UMDF2 sample)                                   */
/* ------------------------------------------------------------------------------------------- */

/*
 * HID_XFER_PACKET has an embedded pointer, which UMDF cannot marshal between processes, so
 * mshidumdf.sys sends the IOCTL_UMDF_HID_* (and READ/WRITE_REPORT) requests with the report in
 * one buffer and the report ID in the other:
 *   - to read from the device (GET_FEATURE, GET_INPUT_REPORT): report buffer = output buffer,
 *     report ID = first byte of the input buffer;
 *   - to write to the device (SET_FEATURE, SET_OUTPUT_REPORT, WRITE_REPORT): report buffer = input
 *     buffer, report ID = length of the output buffer (the driver may not read the output buffer).
 */
static NTSTATUS
CoroomGetPacketToRead(
    _In_ WDFREQUEST Request,
    _Out_ HID_XFER_PACKET *Packet)
{
    WDFMEMORY inputMemory;
    WDFMEMORY outputMemory;
    size_t inputLength;
    size_t outputLength;
    PVOID input;
    NTSTATUS status;

    RtlZeroMemory(Packet, sizeof(*Packet));

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    input = WdfMemoryGetBuffer(inputMemory, &inputLength);
    if (inputLength < sizeof(UCHAR)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    Packet->reportId = *(PUCHAR)input;

    status = WdfRequestRetrieveOutputMemory(Request, &outputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    Packet->reportBuffer = (PUCHAR)WdfMemoryGetBuffer(outputMemory, &outputLength);
    Packet->reportBufferLen = (ULONG)outputLength;
    return STATUS_SUCCESS;
}

static NTSTATUS
CoroomGetPacketToWrite(
    _In_ WDFREQUEST Request,
    _Out_ HID_XFER_PACKET *Packet)
{
    WDFMEMORY inputMemory;
    WDFMEMORY outputMemory;
    size_t inputLength;
    size_t outputLength;
    NTSTATUS status;

    RtlZeroMemory(Packet, sizeof(*Packet));

    status = WdfRequestRetrieveOutputMemory(Request, &outputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    (VOID)WdfMemoryGetBuffer(outputMemory, &outputLength);
    Packet->reportId = (UCHAR)outputLength;

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    Packet->reportBuffer = (PUCHAR)WdfMemoryGetBuffer(inputMemory, &inputLength);
    Packet->reportBufferLen = (ULONG)inputLength;
    return STATUS_SUCCESS;
}

/* Copies Length bytes to the output buffer of the request and sets its information. */
static NTSTATUS
CoroomCompleteWithBytes(
    _In_ WDFREQUEST Request,
    _In_reads_bytes_(Length) const VOID *Source,
    _In_ size_t Length)
{
    WDFMEMORY memory;
    size_t outputLength;
    NTSTATUS status;

    status = WdfRequestRetrieveOutputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    (VOID)WdfMemoryGetBuffer(memory, &outputLength);
    if (outputLength < Length) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    status = WdfMemoryCopyFromBuffer(memory, 0, (PVOID)Source, Length);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WdfRequestSetInformation(Request, (ULONG_PTR)Length);
    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------------------------- */
/* Gamepad state                                                                               */
/* ------------------------------------------------------------------------------------------- */

static VOID
CoroomNeutralReport(
    _Out_writes_bytes_(COROOM_VHID_REPORT_SIZE) UCHAR *Report)
{
    RtlZeroMemory(Report, COROOM_VHID_REPORT_SIZE);
    Report[0] = COROOM_VHID_REPORT_ID;
    Report[COROOM_VHID_REPORT_OFFSET_HAT] = COROOM_VHID_HAT_NULL;
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
    for (i = 0; i < ARRAYSIZE(sticks); i++) {
        if (CoroomReadInt16(Report, sticks[i]) < -COROOM_VHID_AXIS_MAX) {
            return FALSE;
        }
    }
    for (i = 0; i < ARRAYSIZE(triggers); i++) {
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

/*
 * Makes Report the current state and hands it to hidclass: to a read request waiting in the
 * manual queue or, if there is none, to the next one that arrives (Pending). FromClient: it came
 * in a state report (feeds the watchdog); otherwise it is the watchdog putting the gamepad at
 * rest.
 */
static VOID
CoroomPublish(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_reads_bytes_(COROOM_VHID_REPORT_SIZE) const UCHAR *Report,
    _In_ BOOLEAN FromClient)
{
    UCHAR copy[COROOM_VHID_REPORT_SIZE] = {0};
    WDFREQUEST request = NULL;
    NTSTATUS status;

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (FromClient) {
        Context->LastStateTick = GetTickCount64();
        Context->ClientActive = TRUE;
    }
    if (RtlEqualMemory(Context->Report, Report, COROOM_VHID_REPORT_SIZE)) {
        /* Nothing new for hidclass (a keepalive, or already at rest). */
        WdfWaitLockRelease(Context->Lock);
        return;
    }
    RtlCopyMemory(Context->Report, Report, COROOM_VHID_REPORT_SIZE);
    if (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(Context->ReadQueue, &request))) {
        RtlCopyMemory(copy, Context->Report, COROOM_VHID_REPORT_SIZE);
        Context->Pending = FALSE;
    } else {
        request = NULL;
        Context->Pending = TRUE;
    }
    WdfWaitLockRelease(Context->Lock);

    if (request != NULL) {
        status = CoroomCompleteWithBytes(request, copy, COROOM_VHID_REPORT_SIZE);
        WdfRequestComplete(request, status);
    }
}

/* ------------------------------------------------------------------------------------------- */
/* Driver and device                                                                           */
/* ------------------------------------------------------------------------------------------- */

NTSTATUS
DriverEntry(
    _In_ PDRIVER_OBJECT DriverObject,
    _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;

    WDF_DRIVER_CONFIG_INIT(&config, CoroomEvtDeviceAdd);
    return WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config,
                           WDF_NO_HANDLE);
}

/* Slot of this device, written by install.ps1 in its hardware key. */
static NTSTATUS
CoroomReadSlot(
    _In_ WDFDEVICE Device,
    _Out_ UCHAR *Slot)
{
    UNICODE_STRING valueName;
    WDFKEY key;
    ULONG value = 0;
    NTSTATUS status;

    *Slot = 0;
    RtlInitUnicodeString(&valueName, COROOM_VHID_SLOT_VALUE);
    status = WdfDeviceOpenRegistryKey(Device, PLUGPLAY_REGKEY_DEVICE, KEY_READ,
                                      WDF_NO_OBJECT_ATTRIBUTES, &key);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = WdfRegistryQueryULong(key, &valueName, &value);
    WdfRegistryClose(key);
    if (!NT_SUCCESS(status)) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if (value >= COROOM_VHID_MAX_SLOTS) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    *Slot = (UCHAR)value;
    return STATUS_SUCCESS;
}

NTSTATUS
CoroomEvtDeviceAdd(
    _In_ WDFDRIVER Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnpPower;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_TIMER_CONFIG timerConfig;
    PCOROOM_DEVICE_CONTEXT context;
    WDFDEVICE device;
    NTSTATUS status;
    UNREFERENCED_PARAMETER(Driver);

    /* mshidumdf.sys is the function driver: this one is a filter and does not own power policy. */
    WdfFdoInitSetFilter(DeviceInit);

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpPower);
    pnpPower.EvtDeviceD0Entry = CoroomEvtDeviceD0Entry;
    pnpPower.EvtDeviceD0Exit = CoroomEvtDeviceD0Exit;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpPower);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, COROOM_DEVICE_CONTEXT);
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* The framework zeroes the context. */
    context = CoroomGetDeviceContext(device);
    context->Device = device;

    status = CoroomReadSlot(device, &context->Slot);
    if (!NT_SUCCESS(status)) {
        /* Created by hand, without install.ps1: Device Manager shows the error. */
        return status;
    }

    context->Attributes.Size = sizeof(HID_DEVICE_ATTRIBUTES);
    context->Attributes.VendorID = COROOM_VHID_VENDOR_ID;
    context->Attributes.ProductID = COROOM_VHID_PRODUCT_ID;
    context->Attributes.VersionNumber = COROOM_VHID_VERSION_NUMBER;

    context->HidDescriptor.bLength = (UCHAR)sizeof(HID_DESCRIPTOR);
    context->HidDescriptor.bDescriptorType = 0x21; /* HID descriptor */
    context->HidDescriptor.bcdHID = 0x0100;         /* as the vhidmini2 sample */
    context->HidDescriptor.bCountry = 0;
    context->HidDescriptor.bNumDescriptors = 1;
    context->HidDescriptor.DescriptorList[0].bReportType = 0x22; /* report descriptor */
    context->HidDescriptor.DescriptorList[0].wReportLength =
        (USHORT)sizeof(CoroomReportDescriptor);

    RtlCopyMemory(context->Serial, CoroomSerialPrefix, sizeof(CoroomSerialPrefix));
    context->Serial[ARRAYSIZE(CoroomSerialPrefix) - 1] = (WCHAR)(L'1' + context->Slot);
    context->Serial[ARRAYSIZE(CoroomSerialPrefix)] = L'\0';

    /* At rest until the client says otherwise; the first read request gets this state. */
    CoroomNeutralReport(context->Report);
    context->Pending = TRUE;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = device;
    status = WdfWaitLockCreate(&attributes, &context->Lock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Default queue: the HID requests from mshidumdf.sys (DEVICE_CONTROL in UMDF). */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = CoroomEvtIoDeviceControl;
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Manual queue: IOCTL_HID_READ_REPORT waits here for a new state. */
    WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES,
                              &context->ReadQueue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* The watchdog has its own lock discipline: no automatic serialization with the device. */
    WDF_TIMER_CONFIG_INIT_PERIODIC(&timerConfig, CoroomEvtWatchdog, COROOM_WATCHDOG_PERIOD_MS);
    timerConfig.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = device;
    return WdfTimerCreate(&timerConfig, &attributes, &context->Watchdog);
}

NTSTATUS
CoroomEvtDeviceD0Entry(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);

    (VOID)WdfTimerStart(CoroomGetDeviceContext(Device)->Watchdog,
                        WDF_REL_TIMEOUT_IN_MS(COROOM_WATCHDOG_PERIOD_MS));
    return STATUS_SUCCESS;
}

NTSTATUS
CoroomEvtDeviceD0Exit(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    UNREFERENCED_PARAMETER(TargetState);

    /* Waits for a callback in progress; it never runs again until the next D0Entry. */
    (VOID)WdfTimerStop(CoroomGetDeviceContext(Device)->Watchdog, TRUE);
    return STATUS_SUCCESS;
}

VOID
CoroomEvtWatchdog(
    _In_ WDFTIMER Timer)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(WdfTimerGetParentObject(Timer));
    UCHAR neutral[COROOM_VHID_REPORT_SIZE];
    BOOLEAN expired;

    (VOID)WdfWaitLockAcquire(context->Lock, NULL);
    expired = (BOOLEAN)(context->ClientActive &&
                        GetTickCount64() - context->LastStateTick >= COROOM_VHID_WATCHDOG_MS);
    if (expired) {
        context->ClientActive = FALSE;
    }
    WdfWaitLockRelease(context->Lock);

    if (expired) {
        /* The client stopped (closed, crashed or hung): nothing stays pressed. */
        CoroomNeutralReport(neutral);
        CoroomPublish(context, neutral, FALSE);
    }
}

/* ------------------------------------------------------------------------------------------- */
/* HID requests                                                                                */
/* ------------------------------------------------------------------------------------------- */

/* IOCTL_HID_READ_REPORT: the current state if hidclass has not got it yet, otherwise wait. */
static NTSTATUS
CoroomReadReport(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ WDFREQUEST Request,
    _Out_ BOOLEAN *Complete)
{
    UCHAR copy[COROOM_VHID_REPORT_SIZE] = {0};
    BOOLEAN deliver = FALSE;
    NTSTATUS status = STATUS_SUCCESS;

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    if (Context->Pending) {
        RtlCopyMemory(copy, Context->Report, COROOM_VHID_REPORT_SIZE);
        Context->Pending = FALSE;
        deliver = TRUE;
    } else {
        /* Under the lock, so that CoroomPublish cannot miss it. */
        status = WdfRequestForwardToIoQueue(Request, Context->ReadQueue);
    }
    WdfWaitLockRelease(Context->Lock);

    if (deliver) {
        *Complete = TRUE;
        return CoroomCompleteWithBytes(Request, copy, COROOM_VHID_REPORT_SIZE);
    }
    *Complete = (BOOLEAN)!NT_SUCCESS(status);
    return status;
}

/* IOCTL_HID_WRITE_REPORT and IOCTL_UMDF_HID_SET_OUTPUT_REPORT: the state report of Col02. */
static NTSTATUS
CoroomSetState(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ WDFREQUEST Request)
{
    UCHAR report[COROOM_VHID_REPORT_SIZE];
    HID_XFER_PACKET packet;
    NTSTATUS status;

    status = CoroomGetPacketToWrite(Request, &packet);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (packet.reportId != COROOM_VHID_STATE_REPORT_ID) {
        return STATUS_INVALID_PARAMETER;
    }
    /* The buffer starts with the report ID, as the output reports of the vhidmini2 sample. */
    if (packet.reportBufferLen != COROOM_VHID_STATE_REPORT_SIZE || packet.reportBuffer == NULL) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    RtlCopyMemory(report, packet.reportBuffer + 1, COROOM_VHID_REPORT_SIZE);
    if (!CoroomReportIsValid(report)) {
        return STATUS_INVALID_PARAMETER;
    }

    CoroomPublish(Context, report, TRUE);
    WdfRequestSetInformation(Request, COROOM_VHID_STATE_REPORT_SIZE);
    return STATUS_SUCCESS;
}

/* IOCTL_UMDF_HID_GET_FEATURE: the info report of Col02. */
static NTSTATUS
CoroomGetFeature(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ WDFREQUEST Request)
{
    HID_XFER_PACKET packet;
    COROOM_VHID_INFO info;
    NTSTATUS status;

    status = CoroomGetPacketToRead(Request, &packet);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (packet.reportId != COROOM_VHID_INFO_REPORT_ID) {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen < COROOM_VHID_INFO_REPORT_SIZE || packet.reportBuffer == NULL) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    RtlZeroMemory(&info, sizeof(info));
    info.ProtocolVersion = COROOM_VHID_PROTOCOL_VERSION;
    info.Slot = Context->Slot;
    info.ReportSize = COROOM_VHID_REPORT_SIZE;

    /* Written, never read: UMDF only lets the driver write the output buffer. */
    packet.reportBuffer[0] = COROOM_VHID_INFO_REPORT_ID;
    RtlCopyMemory(packet.reportBuffer + 1, &info, sizeof(info));
    WdfRequestSetInformation(Request, COROOM_VHID_INFO_REPORT_SIZE);
    return STATUS_SUCCESS;
}

/* IOCTL_UMDF_HID_GET_INPUT_REPORT: the current state of the gamepad (HidD_GetInputReport). */
static NTSTATUS
CoroomGetInputReport(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ WDFREQUEST Request)
{
    HID_XFER_PACKET packet;
    NTSTATUS status;

    status = CoroomGetPacketToRead(Request, &packet);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (packet.reportId != COROOM_VHID_REPORT_ID) {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen < COROOM_VHID_REPORT_SIZE || packet.reportBuffer == NULL) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    (VOID)WdfWaitLockAcquire(Context->Lock, NULL);
    RtlCopyMemory(packet.reportBuffer, Context->Report, COROOM_VHID_REPORT_SIZE);
    WdfWaitLockRelease(Context->Lock);
    WdfRequestSetInformation(Request, COROOM_VHID_REPORT_SIZE);
    return STATUS_SUCCESS;
}

/* IOCTL_HID_GET_STRING: mshidumdf.sys passes the string ID (low word) in the input buffer. */
static NTSTATUS
CoroomGetString(
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ WDFREQUEST Request)
{
    WDFMEMORY inputMemory;
    size_t inputLength;
    PVOID input;
    ULONG value;
    const WCHAR *string = NULL;
    size_t bytes = 0;
    NTSTATUS status;

    status = WdfRequestRetrieveInputMemory(Request, &inputMemory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    input = WdfMemoryGetBuffer(inputMemory, &inputLength);
    if (inputLength < sizeof(ULONG)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    RtlCopyMemory(&value, input, sizeof(value));

    switch (value & 0xFFFF) {
    case HID_STRING_ID_IMANUFACTURER:
        string = CoroomManufacturer;
        bytes = sizeof(CoroomManufacturer);
        break;
    case HID_STRING_ID_IPRODUCT:
        string = CoroomProduct;
        bytes = sizeof(CoroomProduct);
        break;
    case HID_STRING_ID_ISERIALNUMBER:
        string = Context->Serial;
        bytes = COROOM_SERIAL_CHARS * sizeof(WCHAR);
        break;
    default:
        return STATUS_INVALID_PARAMETER;
    }
    return CoroomCompleteWithBytes(Request, string, bytes);
}

VOID
CoroomEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(WdfIoQueueGetDevice(Queue));
    BOOLEAN complete = TRUE;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        status = CoroomCompleteWithBytes(Request, &context->HidDescriptor,
                                         context->HidDescriptor.bLength);
        break;
    case IOCTL_HID_GET_DEVICE_ATTRIBUTES:
        status = CoroomCompleteWithBytes(Request, &context->Attributes,
                                         sizeof(HID_DEVICE_ATTRIBUTES));
        break;
    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        status = CoroomCompleteWithBytes(Request, CoroomReportDescriptor,
                                         sizeof(CoroomReportDescriptor));
        break;
    case IOCTL_HID_READ_REPORT:
        status = CoroomReadReport(context, Request, &complete);
        break;
    case IOCTL_HID_WRITE_REPORT:
    case IOCTL_UMDF_HID_SET_OUTPUT_REPORT:
        status = CoroomSetState(context, Request);
        break;
    case IOCTL_UMDF_HID_GET_FEATURE:
        status = CoroomGetFeature(context, Request);
        break;
    case IOCTL_UMDF_HID_GET_INPUT_REPORT:
        status = CoroomGetInputReport(context, Request);
        break;
    case IOCTL_HID_GET_STRING:
        status = CoroomGetString(context, Request);
        break;
    case IOCTL_UMDF_HID_SET_FEATURE:
        /* The info report is read only. */
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    case IOCTL_HID_GET_INDEXED_STRING:
    case IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST:
    case IOCTL_HID_ACTIVATE_DEVICE:
    case IOCTL_HID_DEACTIVATE_DEVICE:
    case IOCTL_GET_PHYSICAL_DESCRIPTOR:
        /* Nothing to do for a virtual device (as the vhidmini2 sample). */
    default:
        status = STATUS_NOT_IMPLEMENTED;
        break;
    }

    if (complete) {
        WdfRequestComplete(Request, status);
    }
}
