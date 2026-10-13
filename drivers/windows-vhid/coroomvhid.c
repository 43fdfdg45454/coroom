/*
 * coroomvhid.c - coroom virtual gamepad driver (UMDF2, protocol 3).
 *
 * Two kinds of device, both root devices created by install.ps1 (see coroomvhid.inf):
 *
 *   - The control device (Root\coroomvhid) is a HID source driver of the Virtual HID Framework:
 *     COROOM_VHID_IOCTL_CREATE makes a gamepad with VhfCreate, with the VID/PID, version and
 *     report descriptor of the client (the guest's controller as it is, or the generic gamepad of
 *     report_descriptor.inc). The input reports go to VhfReadReportSubmit (VHF buffers them); the
 *     output and feature reports that Windows sends (vibration, LEDs, modes) wait in a ring until
 *     the client takes them; feature reads are answered with the reports the client gave at
 *     creation, without a round trip. The device is exclusive: one client, and closing its handle
 *     removes its gamepads.
 *   - The Xbox 360 devices (Root\coroomxusb) answer the xusb22 IOCTLs of xinput1_4.dll and
 *     Windows.Gaming.Input with the state the client writes. The layouts of those IOCTLs are not
 *     documented by Microsoft; the values here follow HIDMaestro (MIT License, Copyright (c)
 *     hifihedgehog, driver/companion.c), which checked them against a wired Xbox 360 controller.
 *
 * Rules kept throughout this file:
 *   - every buffer size is checked before the buffer is touched, and the client data is copied
 *     before it is validated;
 *   - requests are completed and VhfDelete is called without a device lock held;
 *   - a gamepad with a rest report, and an active Xbox 360 device, go back to rest when the client
 *     stops sending (COROOM_VHID_WATCHDOG_MS), so nothing stays pressed.
 *
 * It runs in user mode (WUDFHost.exe): a bug here ends that host process, not the system, and the
 * binary needs no kernel-mode signature, only a driver package catalog trusted on the machine.
 */

#include "driver.h"

/* Container IDs of the gamepads: this GUID with the last byte replaced by the pad number, so
   Windows shows each one as its own device. */
static const GUID CoroomContainerBase = {
    0x9a4e3c55, 0x0b5f, 0x4f0c, {0x8f, 0x6d, 0x6c, 0x7a, 0x3e, 0x1b, 0x2d, 0x00}};

/* ------------------------------------------------------------------------------------------- */
/* Helpers                                                                                     */
/* ------------------------------------------------------------------------------------------- */

/* Copies Length bytes to the output buffer of the request and sets its information. */
static NTSTATUS
CoroomCompleteWithBytes(
    _In_ WDFREQUEST Request,
    _In_reads_bytes_(Length) const VOID *Source,
    _In_ size_t Length)
{
    PVOID buffer;
    size_t bufferLength;
    NTSTATUS status;

    status = WdfRequestRetrieveOutputBuffer(Request, Length, &buffer, &bufferLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlCopyMemory(buffer, Source, Length);
    WdfRequestSetInformation(Request, (ULONG_PTR)Length);
    return STATUS_SUCCESS;
}

static USHORT
CoroomReadU16(
    _In_reads_bytes_(2) const UCHAR *Bytes)
{
    return (USHORT)((USHORT)Bytes[0] | (USHORT)((USHORT)Bytes[1] << 8));
}

static VOID
CoroomWriteU16(
    _Out_writes_bytes_(2) UCHAR *Bytes,
    _In_ USHORT Value)
{
    Bytes[0] = (UCHAR)(Value & 0xFF);
    Bytes[1] = (UCHAR)(Value >> 8);
}

static VOID
CoroomWriteU32(
    _Out_writes_bytes_(4) UCHAR *Bytes,
    _In_ ULONG Value)
{
    Bytes[0] = (UCHAR)(Value & 0xFF);
    Bytes[1] = (UCHAR)((Value >> 8) & 0xFF);
    Bytes[2] = (UCHAR)((Value >> 16) & 0xFF);
    Bytes[3] = (UCHAR)(Value >> 24);
}

/* Whether a report descriptor uses report IDs (a Report ID global item, tag 0x84). */
static BOOLEAN
CoroomDescriptorHasReportIds(
    _In_reads_bytes_(Length) const UCHAR *Descriptor,
    _In_ USHORT Length)
{
    USHORT at = 0;

    while (at < Length) {
        UCHAR prefix = Descriptor[at];
        USHORT size;

        if (prefix == 0xFE) {
            /* Long item: size and tag follow. */
            if ((ULONG)at + 2 >= Length) {
                return FALSE;
            }
            at = (USHORT)(at + 3 + Descriptor[at + 1]);
            continue;
        }
        size = (USHORT)((prefix & 0x03) == 3 ? 4 : (prefix & 0x03));
        if ((prefix & 0xFC) == 0x84) {
            return TRUE;
        }
        at = (USHORT)(at + 1 + size);
    }
    return FALSE;
}

/* ------------------------------------------------------------------------------------------- */
/* Driver and devices                                                                          */
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

/* A DWORD of the hardware key of the device, before WdfDeviceCreate. */
static NTSTATUS
CoroomReadInitValue(
    _In_ PWDFDEVICE_INIT DeviceInit,
    _In_ PCWSTR Name,
    _Out_ ULONG *Value)
{
    UNICODE_STRING valueName;
    WDFKEY key;
    NTSTATUS status;

    *Value = 0;
    RtlInitUnicodeString(&valueName, Name);
    status = WdfFdoInitOpenRegistryKey(DeviceInit, PLUGPLAY_REGKEY_DEVICE, KEY_READ,
                                       WDF_NO_OBJECT_ATTRIBUTES, &key);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = WdfRegistryQueryULong(key, &valueName, Value);
    WdfRegistryClose(key);
    return status;
}

static NTSTATUS CoroomAddControl(_In_ WDFDEVICE Device, _In_ PCOROOM_DEVICE_CONTEXT Context);
static VOID CoroomXusbDeactivate(_In_ PCOROOM_XUSB_CONTEXT Xusb);
static NTSTATUS CoroomAddXusb(_In_ WDFDEVICE Device, _In_ PCOROOM_DEVICE_CONTEXT Context,
                              _In_ UCHAR Slot);

NTSTATUS
CoroomEvtDeviceAdd(
    _In_ WDFDRIVER Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnpPower;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_OBJECT_ATTRIBUTES attributes;
    PCOROOM_DEVICE_CONTEXT context;
    WDFDEVICE device;
    ULONG kind = 0;
    ULONG slot = 0;
    NTSTATUS status;
    UNREFERENCED_PARAMETER(Driver);

    status = CoroomReadInitValue(DeviceInit, COROOM_KIND_VALUE, &kind);
    if (!NT_SUCCESS(status) ||
        (kind != COROOM_VHID_KIND_CONTROL && kind != COROOM_VHID_KIND_XUSB)) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if (kind == COROOM_VHID_KIND_XUSB) {
        /* Its slot, written by install.ps1: without it, Device Manager shows the error. */
        status = CoroomReadInitValue(DeviceInit, COROOM_VHID_SLOT_VALUE, &slot);
        if (!NT_SUCCESS(status) || slot >= COROOM_VHID_XUSB_SLOTS) {
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }
    /* The control device is exclusive (Exclusive = 1 in the INF): one client at a time, and its
       handle owns the gamepads. */

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpPower);
    pnpPower.EvtDeviceD0Entry = CoroomEvtDeviceD0Entry;
    pnpPower.EvtDeviceD0Exit = CoroomEvtDeviceD0Exit;
    pnpPower.EvtDeviceSelfManagedIoInit = CoroomEvtSelfManagedIoInit;
    pnpPower.EvtDeviceSelfManagedIoCleanup = CoroomEvtSelfManagedIoCleanup;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpPower);

    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, WDF_NO_EVENT_CALLBACK, WDF_NO_EVENT_CALLBACK,
                               CoroomEvtFileCleanup);
    WdfDeviceInitSetFileObjectConfig(DeviceInit, &fileConfig, WDF_NO_OBJECT_ATTRIBUTES);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, COROOM_DEVICE_CONTEXT);
    attributes.EvtCleanupCallback = CoroomEvtDeviceCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* The framework zeroes the context. */
    context = CoroomGetDeviceContext(device);
    context->Kind = (UCHAR)kind;

    status = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_COROOM_VHID, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (kind == COROOM_VHID_KIND_XUSB) {
        return CoroomAddXusb(device, context, (UCHAR)slot);
    }
    return CoroomAddControl(device, context);
}

VOID
CoroomEvtDeviceCleanup(
    _In_ WDFOBJECT Object)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext((WDFDEVICE)Object);

    /* The gamepads are gone by now (file cleanup, or the device was never opened). */
    free(context->Control);
    context->Control = NULL;
    free(context->Xusb);
    context->Xusb = NULL;
}

NTSTATUS
CoroomEvtDeviceD0Entry(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE PreviousState)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(Device);
    UNREFERENCED_PARAMETER(PreviousState);

    if (context->Control != NULL) {
        (VOID)WdfTimerStart(context->Control->Watchdog,
                            WDF_REL_TIMEOUT_IN_MS(COROOM_WATCHDOG_PERIOD_MS));
    }
    if (context->Xusb != NULL) {
        (VOID)WdfTimerStart(context->Xusb->Watchdog,
                            WDF_REL_TIMEOUT_IN_MS(COROOM_WATCHDOG_PERIOD_MS));
        (VOID)WdfTimerStart(context->Xusb->Pump, WDF_REL_TIMEOUT_IN_MS(COROOM_XUSB_PUMP_MS));
    }
    return STATUS_SUCCESS;
}

NTSTATUS
CoroomEvtDeviceD0Exit(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(Device);
    UNREFERENCED_PARAMETER(TargetState);

    /* Each one waits for a callback in progress and does not run again until D0Entry. */
    if (context->Control != NULL) {
        (VOID)WdfTimerStop(context->Control->Watchdog, TRUE);
    }
    if (context->Xusb != NULL) {
        (VOID)WdfTimerStop(context->Xusb->Watchdog, TRUE);
        (VOID)WdfTimerStop(context->Xusb->Pump, TRUE);
    }
    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------------------------- */
/* Control device                                                                              */
/* ------------------------------------------------------------------------------------------- */

static NTSTATUS
CoroomAddControl(
    _In_ WDFDEVICE Device,
    _In_ PCOROOM_DEVICE_CONTEXT Context)
{
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_TIMER_CONFIG timerConfig;
    PCOROOM_CONTROL_CONTEXT control;
    UCHAR i;
    NTSTATUS status;

    control = (PCOROOM_CONTROL_CONTEXT)malloc(sizeof(*control));
    if (control == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(control, sizeof(*control));
    control->Device = Device;
    for (i = 0; i < COROOM_VHID_MAX_PADS; i++) {
        control->Pads[i].Control = control;
        control->Pads[i].Number = i;
    }
    Context->Control = control;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    status = WdfWaitLockCreate(&attributes, &control->Lock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Sequential: creating, feeding and destroying gamepads never overlap. */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchSequential);
    queueConfig.EvtIoDeviceControl = CoroomEvtControlIoctl;
    status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_TIMER_CONFIG_INIT_PERIODIC(&timerConfig, CoroomEvtControlWatchdog,
                                   COROOM_WATCHDOG_PERIOD_MS);
    timerConfig.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    return WdfTimerCreate(&timerConfig, &attributes, &control->Watchdog);
}

/* Removes a gamepad: no more callbacks once VhfDelete returns, then its memory goes. */
static VOID
CoroomDestroyPad(
    _In_ PCOROOM_PAD Pad)
{
    PCOROOM_CONTROL_CONTEXT control = Pad->Control;
    VHFHANDLE vhf;
    WDFIOTARGET target;
    UCHAR *descriptor;
    UCHAR *features;

    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    Pad->Live = FALSE;
    vhf = Pad->Vhf;
    Pad->Vhf = NULL;
    target = Pad->Target;
    Pad->Target = NULL;
    WdfWaitLockRelease(control->Lock);

    if (vhf != NULL) {
        VhfDelete(vhf, TRUE);
    }
    if (target != NULL) {
        WdfObjectDelete(target);
    }

    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    descriptor = Pad->Descriptor;
    Pad->Descriptor = NULL;
    Pad->DescriptorLength = 0;
    features = Pad->Features;
    Pad->Features = NULL;
    Pad->FeaturesLength = 0;
    Pad->NeutralLength = 0;
    Pad->LastInputLength = 0;
    WdfWaitLockRelease(control->Lock);
    free(descriptor);
    free(features);
}

static VOID
CoroomDestroyAllPads(
    _In_ PCOROOM_CONTROL_CONTEXT Control)
{
    UCHAR i;

    for (i = 0; i < COROOM_VHID_MAX_PADS; i++) {
        CoroomDestroyPad(&Control->Pads[i]);
    }
}

/* The feature blob is a sequence of UINT16 lengths, each followed by a report. */
static BOOLEAN
CoroomFeaturesAreValid(
    _In_reads_bytes_(Length) const UCHAR *Features,
    _In_ USHORT Length)
{
    ULONG at = 0;
    ULONG count = 0;

    while (at < Length) {
        USHORT size;

        if (at + 2 > Length || ++count > COROOM_VHID_MAX_FEATURES) {
            return FALSE;
        }
        size = CoroomReadU16(Features + at);
        if (size == 0 || size > COROOM_VHID_MAX_REPORT || at + 2 + size > Length) {
            return FALSE;
        }
        at += 2u + size;
    }
    return TRUE;
}

static NTSTATUS
CoroomCreatePad(
    _In_ PCOROOM_CONTROL_CONTEXT Control,
    _In_ WDFREQUEST Request)
{
    COROOM_VHID_CREATE header;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_IO_TARGET_OPEN_PARAMS openParams;
    VHF_CONFIG config;
    PCOROOM_PAD pad;
    PVOID input;
    size_t inputLength;
    const UCHAR *data;
    UCHAR *descriptor = NULL;
    UCHAR *features = NULL;
    WDFIOTARGET target = NULL;
    VHFHANDLE vhf = NULL;
    GUID container;
    NTSTATUS status;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(header), &input, &inputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlCopyMemory(&header, input, sizeof(header));
    if (header.Pad >= COROOM_VHID_MAX_PADS || header.DescriptorLength == 0 ||
        header.DescriptorLength > COROOM_VHID_MAX_DESCRIPTOR ||
        header.NeutralLength > COROOM_VHID_MAX_REPORT ||
        header.FeaturesLength >
            COROOM_VHID_MAX_FEATURE_BYTES + 2 * COROOM_VHID_MAX_FEATURES ||
        inputLength != sizeof(header) + (size_t)header.DescriptorLength +
                           header.NeutralLength + header.FeaturesLength) {
        return STATUS_INVALID_PARAMETER;
    }
    data = (const UCHAR *)input + sizeof(header);

    descriptor = (UCHAR *)malloc(header.DescriptorLength);
    if (descriptor == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlCopyMemory(descriptor, data, header.DescriptorLength);
    if (header.FeaturesLength > 0) {
        features = (UCHAR *)malloc(header.FeaturesLength);
        if (features == NULL) {
            free(descriptor);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlCopyMemory(features, data + header.DescriptorLength + header.NeutralLength,
                      header.FeaturesLength);
        if (!CoroomFeaturesAreValid(features, header.FeaturesLength)) {
            free(features);
            free(descriptor);
            return STATUS_INVALID_PARAMETER;
        }
    }

    pad = &Control->Pads[header.Pad];
    /* Creating a pad that exists replaces it. */
    CoroomDestroyPad(pad);

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Control->Device;
    status = WdfIoTargetCreate(Control->Device, &attributes, &target);
    if (!NT_SUCCESS(status)) {
        goto fail;
    }
    WDF_IO_TARGET_OPEN_PARAMS_INIT_OPEN_BY_FILE(&openParams, NULL);
    status = WdfIoTargetOpen(target, &openParams);
    if (!NT_SUCCESS(status)) {
        goto fail;
    }

    /* Ready for the callbacks before the device starts: hidclass reads features right away. */
    (VOID)WdfWaitLockAcquire(Control->Lock, NULL);
    pad->Descriptor = descriptor;
    pad->DescriptorLength = header.DescriptorLength;
    pad->Numbered = CoroomDescriptorHasReportIds(descriptor, header.DescriptorLength);
    pad->Features = features;
    pad->FeaturesLength = header.FeaturesLength;
    RtlCopyMemory(pad->Neutral, data + header.DescriptorLength, header.NeutralLength);
    pad->NeutralLength = header.NeutralLength;
    pad->LastInputLength = 0;
    pad->LastInputTick = GetTickCount64();
    pad->Resting = FALSE;
    pad->Target = target;
    pad->Live = TRUE;
    WdfWaitLockRelease(Control->Lock);
    descriptor = NULL;
    features = NULL;
    target = NULL;

    container = CoroomContainerBase;
    container.Data4[7] = header.Pad;

    VHF_CONFIG_INIT(&config, WdfIoTargetWdmGetTargetFileHandle(pad->Target),
                    pad->DescriptorLength, pad->Descriptor);
    config.VhfClientContext = pad;
    config.VendorID = header.VendorId;
    config.ProductID = header.ProductId;
    config.VersionNumber = header.VersionNumber;
    config.ContainerID = container;
    config.EvtVhfAsyncOperationGetFeature = CoroomEvtVhfGetFeature;
    config.EvtVhfAsyncOperationSetFeature = CoroomEvtVhfSetFeature;
    config.EvtVhfAsyncOperationWriteReport = CoroomEvtVhfWriteReport;
    config.EvtVhfAsyncOperationGetInputReport = CoroomEvtVhfGetInputReport;

    status = VhfCreate(&config, &vhf);
    if (!NT_SUCCESS(status)) {
        CoroomDestroyPad(pad);
        return status;
    }
    (VOID)WdfWaitLockAcquire(Control->Lock, NULL);
    pad->Vhf = vhf;
    WdfWaitLockRelease(Control->Lock);

    status = VhfStart(vhf);
    if (!NT_SUCCESS(status)) {
        CoroomDestroyPad(pad);
        return status;
    }
    return STATUS_SUCCESS;

fail:
    if (target != NULL) {
        WdfObjectDelete(target);
    }
    free(features);
    free(descriptor);
    return status;
}

static NTSTATUS
CoroomDestroyRequest(
    _In_ PCOROOM_CONTROL_CONTEXT Control,
    _In_ WDFREQUEST Request)
{
    PVOID input;
    size_t inputLength;
    UCHAR number;
    NTSTATUS status;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(UCHAR), &input, &inputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    number = *(const UCHAR *)input;
    if (inputLength != sizeof(UCHAR) || number >= COROOM_VHID_MAX_PADS) {
        return STATUS_INVALID_PARAMETER;
    }
    CoroomDestroyPad(&Control->Pads[number]);
    return STATUS_SUCCESS;
}

/* Hands a report to VHF. Called with the control lock held (VhfReadReportSubmit does not wait
   and does not call back into this driver). */
static NTSTATUS
CoroomSubmitLocked(
    _In_ PCOROOM_PAD Pad,
    _In_reads_bytes_(Length) UCHAR *Report,
    _In_ USHORT Length)
{
    HID_XFER_PACKET packet;

    if (!Pad->Live || Pad->Vhf == NULL) {
        return STATUS_DEVICE_NOT_CONNECTED;
    }
    packet.reportBuffer = Report;
    packet.reportBufferLen = Length;
    packet.reportId = Pad->Numbered ? Report[0] : 0;
    return VhfReadReportSubmit(Pad->Vhf, &packet);
}

static NTSTATUS
CoroomInput(
    _In_ PCOROOM_CONTROL_CONTEXT Control,
    _In_ WDFREQUEST Request)
{
    COROOM_VHID_INPUT header;
    PCOROOM_PAD pad;
    PVOID input;
    size_t inputLength;
    NTSTATUS status;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(header), &input, &inputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlCopyMemory(&header, input, sizeof(header));
    if (header.Pad >= COROOM_VHID_MAX_PADS || header.Length == 0 ||
        header.Length > COROOM_VHID_MAX_REPORT ||
        inputLength != sizeof(header) + (size_t)header.Length) {
        return STATUS_INVALID_PARAMETER;
    }
    pad = &Control->Pads[header.Pad];

    (VOID)WdfWaitLockAcquire(Control->Lock, NULL);
    RtlCopyMemory(pad->LastInput, (const UCHAR *)input + sizeof(header), header.Length);
    pad->LastInputLength = header.Length;
    pad->LastInputTick = GetTickCount64();
    pad->Resting = FALSE;
    status = CoroomSubmitLocked(pad, pad->LastInput, header.Length);
    WdfWaitLockRelease(Control->Lock);
    return status;
}

static NTSTATUS
CoroomTakeOutput(
    _In_ PCOROOM_CONTROL_CONTEXT Control,
    _In_ WDFREQUEST Request)
{
    COROOM_OUTPUT_ENTRY entry = {0};
    PVOID buffer;
    size_t bufferLength;
    BOOLEAN found = FALSE;
    size_t length;
    NTSTATUS status;

    status = WdfRequestRetrieveOutputBuffer(Request,
                                            COROOM_VHID_OUTPUT_HEADER_SIZE + COROOM_VHID_MAX_REPORT,
                                            &buffer, &bufferLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    (VOID)WdfWaitLockAcquire(Control->Lock, NULL);
    if (Control->OutputCount > 0) {
        RtlCopyMemory(&entry, &Control->Outputs[Control->OutputHead], sizeof(entry));
        Control->OutputHead = (Control->OutputHead + 1) % COROOM_VHID_MAX_OUTPUTS;
        Control->OutputCount--;
        found = TRUE;
    }
    WdfWaitLockRelease(Control->Lock);

    if (!found) {
        WdfRequestSetInformation(Request, 0);
        return STATUS_SUCCESS;
    }
    length = COROOM_VHID_OUTPUT_HEADER_SIZE + (size_t)entry.Header.Length;
    RtlCopyMemory(buffer, &entry.Header, COROOM_VHID_OUTPUT_HEADER_SIZE);
    RtlCopyMemory((UCHAR *)buffer + COROOM_VHID_OUTPUT_HEADER_SIZE, entry.Data,
                  entry.Header.Length);
    WdfRequestSetInformation(Request, (ULONG_PTR)length);
    return STATUS_SUCCESS;
}

VOID
CoroomEvtControlIoctl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    PCOROOM_CONTROL_CONTEXT control =
        CoroomGetDeviceContext(WdfIoQueueGetDevice(Queue))->Control;
    COROOM_VHID_INFO info;
    UCHAR i;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case COROOM_VHID_IOCTL_GET_INFO:
        RtlZeroMemory(&info, sizeof(info));
        info.ProtocolVersion = COROOM_VHID_PROTOCOL_VERSION;
        info.Kind = COROOM_VHID_KIND_CONTROL;
        info.Slot = COROOM_VHID_MAX_PADS;
        (VOID)WdfWaitLockAcquire(control->Lock, NULL);
        for (i = 0; i < COROOM_VHID_MAX_PADS; i++) {
            info.Active = (UCHAR)(info.Active + (control->Pads[i].Live ? 1 : 0));
        }
        WdfWaitLockRelease(control->Lock);
        status = CoroomCompleteWithBytes(Request, &info, sizeof(info));
        break;
    case COROOM_VHID_IOCTL_CREATE:
        status = CoroomCreatePad(control, Request);
        break;
    case COROOM_VHID_IOCTL_DESTROY:
        status = CoroomDestroyRequest(control, Request);
        break;
    case COROOM_VHID_IOCTL_INPUT:
        status = CoroomInput(control, Request);
        break;
    case COROOM_VHID_IOCTL_TAKE_OUTPUT:
        status = CoroomTakeOutput(control, Request);
        break;
    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }
    WdfRequestComplete(Request, status);
}

VOID
CoroomEvtControlWatchdog(
    _In_ WDFTIMER Timer)
{
    PCOROOM_CONTROL_CONTEXT control =
        CoroomGetDeviceContext((WDFDEVICE)WdfTimerGetParentObject(Timer))->Control;
    ULONGLONG now = GetTickCount64();
    UCHAR i;

    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    for (i = 0; i < COROOM_VHID_MAX_PADS; i++) {
        PCOROOM_PAD pad = &control->Pads[i];

        if (pad->Live && pad->NeutralLength > 0 && !pad->Resting &&
            now - pad->LastInputTick >= COROOM_VHID_WATCHDOG_MS) {
            /* The client stopped (hung or too slow): nothing stays pressed. */
            pad->Resting = TRUE;
            (VOID)CoroomSubmitLocked(pad, pad->Neutral, pad->NeutralLength);
        }
    }
    WdfWaitLockRelease(control->Lock);
}

/* ------------------------------------------------------------------------------------------- */
/* VHF callbacks of the gamepads                                                               */
/* ------------------------------------------------------------------------------------------- */

/* Keeps an output for the client (the oldest one goes when the ring is full). */
static NTSTATUS
CoroomPushOutput(
    _In_ PCOROOM_PAD Pad,
    _In_ UCHAR Type,
    _In_ PHID_XFER_PACKET Packet)
{
    PCOROOM_CONTROL_CONTEXT control = Pad->Control;
    COROOM_OUTPUT_ENTRY *entry;
    ULONG index;
    USHORT length;

    if (Packet == NULL || (Packet->reportBuffer == NULL && Packet->reportBufferLen > 0) ||
        Packet->reportBufferLen > COROOM_VHID_MAX_REPORT) {
        return STATUS_INVALID_PARAMETER;
    }
    length = (USHORT)Packet->reportBufferLen;

    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    if (!Pad->Live) {
        WdfWaitLockRelease(control->Lock);
        return STATUS_DEVICE_NOT_CONNECTED;
    }
    if (control->OutputCount == COROOM_VHID_MAX_OUTPUTS) {
        control->OutputHead = (control->OutputHead + 1) % COROOM_VHID_MAX_OUTPUTS;
        control->OutputCount--;
    }
    index = (control->OutputHead + control->OutputCount) % COROOM_VHID_MAX_OUTPUTS;
    entry = &control->Outputs[index];
    RtlZeroMemory(&entry->Header, sizeof(entry->Header));
    entry->Header.Pad = Pad->Number;
    entry->Header.Type = Type;
    entry->Header.ReportId = Packet->reportId;
    entry->Header.Length = length;
    if (length > 0) {
        RtlCopyMemory(entry->Data, Packet->reportBuffer, length);
    }
    control->OutputCount++;
    WdfWaitLockRelease(control->Lock);
    return STATUS_SUCCESS;
}

VOID
CoroomEvtVhfWriteReport(
    _In_ PVOID VhfClientContext,
    _In_ VHFOPERATIONHANDLE VhfOperationHandle,
    _In_opt_ PVOID VhfOperationContext,
    _In_ PHID_XFER_PACKET HidTransferPacket)
{
    UNREFERENCED_PARAMETER(VhfOperationContext);
    VhfAsyncOperationComplete(VhfOperationHandle,
                              CoroomPushOutput((PCOROOM_PAD)VhfClientContext,
                                               COROOM_VHID_OUTPUT_REPORT, HidTransferPacket));
}

VOID
CoroomEvtVhfSetFeature(
    _In_ PVOID VhfClientContext,
    _In_ VHFOPERATIONHANDLE VhfOperationHandle,
    _In_opt_ PVOID VhfOperationContext,
    _In_ PHID_XFER_PACKET HidTransferPacket)
{
    UNREFERENCED_PARAMETER(VhfOperationContext);
    VhfAsyncOperationComplete(VhfOperationHandle,
                              CoroomPushOutput((PCOROOM_PAD)VhfClientContext,
                                               COROOM_VHID_OUTPUT_FEATURE, HidTransferPacket));
}

/* Answers a feature read with the report the client gave for that ID. */
VOID
CoroomEvtVhfGetFeature(
    _In_ PVOID VhfClientContext,
    _In_ VHFOPERATIONHANDLE VhfOperationHandle,
    _In_opt_ PVOID VhfOperationContext,
    _In_ PHID_XFER_PACKET HidTransferPacket)
{
    PCOROOM_PAD pad = (PCOROOM_PAD)VhfClientContext;
    PCOROOM_CONTROL_CONTEXT control = pad->Control;
    NTSTATUS status = STATUS_NOT_SUPPORTED;
    ULONG at = 0;
    UCHAR id;
    UNREFERENCED_PARAMETER(VhfOperationContext);

    if (HidTransferPacket == NULL || HidTransferPacket->reportBuffer == NULL) {
        VhfAsyncOperationComplete(VhfOperationHandle, STATUS_INVALID_PARAMETER);
        return;
    }
    id = HidTransferPacket->reportId;
    if (id == 0 && HidTransferPacket->reportBufferLen > 0) {
        id = HidTransferPacket->reportBuffer[0];
    }

    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    while (pad->Live && pad->Features != NULL && at + 2 <= pad->FeaturesLength) {
        USHORT size = CoroomReadU16(pad->Features + at);
        const UCHAR *report = pad->Features + at + 2;

        if (report[0] == id) {
            if (HidTransferPacket->reportBufferLen < size) {
                status = STATUS_BUFFER_TOO_SMALL;
            } else {
                RtlZeroMemory(HidTransferPacket->reportBuffer,
                              HidTransferPacket->reportBufferLen);
                RtlCopyMemory(HidTransferPacket->reportBuffer, report, size);
                status = STATUS_SUCCESS;
            }
            break;
        }
        at += 2u + size;
    }
    WdfWaitLockRelease(control->Lock);
    VhfAsyncOperationComplete(VhfOperationHandle, status);
}

/* HidD_GetInputReport: the last input report the client sent, if it has that ID. */
VOID
CoroomEvtVhfGetInputReport(
    _In_ PVOID VhfClientContext,
    _In_ VHFOPERATIONHANDLE VhfOperationHandle,
    _In_opt_ PVOID VhfOperationContext,
    _In_ PHID_XFER_PACKET HidTransferPacket)
{
    PCOROOM_PAD pad = (PCOROOM_PAD)VhfClientContext;
    PCOROOM_CONTROL_CONTEXT control = pad->Control;
    NTSTATUS status = STATUS_NOT_SUPPORTED;
    UNREFERENCED_PARAMETER(VhfOperationContext);

    if (HidTransferPacket == NULL || HidTransferPacket->reportBuffer == NULL) {
        VhfAsyncOperationComplete(VhfOperationHandle, STATUS_INVALID_PARAMETER);
        return;
    }
    (VOID)WdfWaitLockAcquire(control->Lock, NULL);
    if (pad->Live && pad->LastInputLength > 0 &&
        (!pad->Numbered || pad->LastInput[0] == HidTransferPacket->reportId)) {
        /* Without report IDs, Windows may leave room for a leading zero. */
        ULONG lead = (!pad->Numbered &&
                      HidTransferPacket->reportBufferLen > pad->LastInputLength) ? 1 : 0;

        if (HidTransferPacket->reportBufferLen < pad->LastInputLength + lead) {
            status = STATUS_BUFFER_TOO_SMALL;
        } else {
            RtlZeroMemory(HidTransferPacket->reportBuffer, HidTransferPacket->reportBufferLen);
            RtlCopyMemory(HidTransferPacket->reportBuffer + lead, pad->LastInput,
                          pad->LastInputLength);
            status = STATUS_SUCCESS;
        }
    }
    WdfWaitLockRelease(control->Lock);
    VhfAsyncOperationComplete(VhfOperationHandle, status);
}

/* ------------------------------------------------------------------------------------------- */
/* Xbox 360 devices                                                                            */
/* ------------------------------------------------------------------------------------------- */

/* xusb22.sys IOCTLs (undocumented; values as HIDMaestro, see the top of this file). */
#define IOCTL_XUSB_GET_INFORMATION 0x80006000
#define IOCTL_XUSB_GET_CAPABILITIES 0x8000E004
#define IOCTL_XUSB_GET_LED_STATE 0x8000E008
#define IOCTL_XUSB_GET_STATE 0x8000E00C
#define IOCTL_XUSB_SET_STATE 0x8000A010
#define IOCTL_XUSB_WAIT_GUIDE 0x8000E014
#define IOCTL_XUSB_GET_BATTERY_INFO 0x8000E018
#define IOCTL_XUSB_POWER_INFO 0x80006380
#define IOCTL_XUSB_WAIT_FOR_INPUT 0x8000E3AC
#define IOCTL_XUSB_GET_INFORMATION_EX 0x8000E3FC

/* XUSB protocol version of a wired Xbox 360 controller (0x0103). */
#define COROOM_XUSB_VERSION 0x0103
#define COROOM_XUSB_STATE_SIZE 29

static NTSTATUS
CoroomAddXusb(
    _In_ WDFDEVICE Device,
    _In_ PCOROOM_DEVICE_CONTEXT Context,
    _In_ UCHAR Slot)
{
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_TIMER_CONFIG timerConfig;
    PCOROOM_XUSB_CONTEXT xusb;
    NTSTATUS status;

    xusb = (PCOROOM_XUSB_CONTEXT)malloc(sizeof(*xusb));
    if (xusb == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(xusb, sizeof(*xusb));
    xusb->Device = Device;
    xusb->Slot = Slot;
    Context->Xusb = xusb;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    status = WdfWaitLockCreate(&attributes, &xusb->Lock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = CoroomEvtXusbIoctl;
    status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);
    status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES,
                              &xusb->WaitQueue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_TIMER_CONFIG_INIT_PERIODIC(&timerConfig, CoroomEvtXusbWatchdog,
                                   COROOM_WATCHDOG_PERIOD_MS);
    timerConfig.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    status = WdfTimerCreate(&timerConfig, &attributes, &xusb->Watchdog);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WDF_TIMER_CONFIG_INIT_PERIODIC(&timerConfig, CoroomEvtXusbPump, COROOM_XUSB_PUMP_MS);
    timerConfig.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    status = WdfTimerCreate(&timerConfig, &attributes, &xusb->Pump);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Enabled at start by the framework; CoroomEvtSelfManagedIoInit turns it off until a client
       activates the slot. */
    return WdfDeviceCreateDeviceInterface(Device, &GUID_DEVINTERFACE_COROOM_XUSB, NULL);
}

NTSTATUS
CoroomEvtSelfManagedIoInit(
    _In_ WDFDEVICE Device)
{
    PCOROOM_XUSB_CONTEXT xusb = CoroomGetDeviceContext(Device)->Xusb;
    BOOLEAN active;

    if (xusb == NULL) {
        return STATUS_SUCCESS;
    }
    (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
    xusb->Started = TRUE;
    active = (BOOLEAN)(xusb->Owner != NULL);
    WdfWaitLockRelease(xusb->Lock);
    WdfDeviceSetDeviceInterfaceState(Device, &GUID_DEVINTERFACE_COROOM_XUSB, NULL, active);
    return STATUS_SUCCESS;
}

/* The device is going away: its gamepads and its Xbox 360 slot go first, while their WDF objects
   still exist. */
VOID
CoroomEvtSelfManagedIoCleanup(
    _In_ WDFDEVICE Device)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(Device);

    if (context->Control != NULL) {
        CoroomDestroyAllPads(context->Control);
    }
    if (context->Xusb != NULL) {
        CoroomXusbDeactivate(context->Xusb);
    }
}

/* XINPUT_GAMEPAD at offset 11 of the 29-byte state, the layout of GET_STATE and WAIT_FOR_INPUT. */
static VOID
CoroomXusbPutGamepad(
    _Out_writes_bytes_(COROOM_XUSB_STATE_SIZE) UCHAR *State,
    _In_ const COROOM_VHID_XUSB_STATE *Gamepad)
{
    CoroomWriteU16(State + 0x0B, Gamepad->Buttons);
    State[0x0D] = Gamepad->LeftTrigger;
    State[0x0E] = Gamepad->RightTrigger;
    CoroomWriteU16(State + 0x0F, (USHORT)Gamepad->ThumbLX);
    CoroomWriteU16(State + 0x11, (USHORT)Gamepad->ThumbLY);
    CoroomWriteU16(State + 0x13, (USHORT)Gamepad->ThumbRX);
    CoroomWriteU16(State + 0x15, (USHORT)Gamepad->ThumbRY);
}

/* The 29-byte state: ForWait builds the WAIT_FOR_INPUT variant that Windows.Gaming.Input reads
   (state 3 "resumed", payload marker 0x14 at offset 10). */
static VOID
CoroomXusbBuildState(
    _In_ PCOROOM_XUSB_CONTEXT Xusb,
    _In_ BOOLEAN ForWait,
    _Out_writes_bytes_(COROOM_XUSB_STATE_SIZE) UCHAR *State)
{
    RtlZeroMemory(State, COROOM_XUSB_STATE_SIZE);
    CoroomWriteU16(State, COROOM_XUSB_VERSION);
    State[2] = ForWait ? 0x03 : 0x01;
    (VOID)WdfWaitLockAcquire(Xusb->Lock, NULL);
    CoroomWriteU32(State + 5, Xusb->PacketNumber);
    CoroomXusbPutGamepad(State, &Xusb->State);
    WdfWaitLockRelease(Xusb->Lock);
    if (ForWait) {
        State[9] = 0x00;
        State[10] = 0x14;
    }
}

/* Completes one parked WAIT_FOR_INPUT with the current state. */
static VOID
CoroomXusbPumpOnce(
    _In_ PCOROOM_XUSB_CONTEXT Xusb)
{
    UCHAR state[COROOM_XUSB_STATE_SIZE];
    WDFREQUEST request;
    NTSTATUS status;

    if (!NT_SUCCESS(WdfIoQueueRetrieveNextRequest(Xusb->WaitQueue, &request))) {
        return;
    }
    CoroomXusbBuildState(Xusb, TRUE, state);
    status = CoroomCompleteWithBytes(request, state, sizeof(state));
    WdfRequestComplete(request, status);
}

VOID
CoroomEvtXusbPump(
    _In_ WDFTIMER Timer)
{
    PCOROOM_XUSB_CONTEXT xusb =
        CoroomGetDeviceContext((WDFDEVICE)WdfTimerGetParentObject(Timer))->Xusb;
    BOOLEAN active;

    (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
    active = (BOOLEAN)(xusb->Owner != NULL);
    WdfWaitLockRelease(xusb->Lock);
    if (active) {
        CoroomXusbPumpOnce(xusb);
    }
}

VOID
CoroomEvtXusbWatchdog(
    _In_ WDFTIMER Timer)
{
    PCOROOM_XUSB_CONTEXT xusb =
        CoroomGetDeviceContext((WDFDEVICE)WdfTimerGetParentObject(Timer))->Xusb;

    (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
    if (xusb->Owner != NULL && !xusb->Resting &&
        GetTickCount64() - xusb->LastStateTick >= COROOM_VHID_WATCHDOG_MS) {
        /* The client stopped: at rest, nothing pressed. */
        RtlZeroMemory(&xusb->State, sizeof(xusb->State));
        xusb->PacketNumber++;
        xusb->Resting = TRUE;
    }
    WdfWaitLockRelease(xusb->Lock);
}

static VOID
CoroomXusbDeactivate(
    _In_ PCOROOM_XUSB_CONTEXT Xusb)
{
    WDFREQUEST request;
    BOOLEAN started;

    (VOID)WdfWaitLockAcquire(Xusb->Lock, NULL);
    Xusb->Owner = NULL;
    RtlZeroMemory(&Xusb->State, sizeof(Xusb->State));
    started = Xusb->Started;
    WdfWaitLockRelease(Xusb->Lock);

    if (started) {
        WdfDeviceSetDeviceInterfaceState(Xusb->Device, &GUID_DEVINTERFACE_COROOM_XUSB, NULL,
                                         FALSE);
    }
    while (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(Xusb->WaitQueue, &request))) {
        WdfRequestComplete(request, STATUS_DEVICE_NOT_CONNECTED);
    }
}

static NTSTATUS
CoroomXusbActivate(
    _In_ PCOROOM_XUSB_CONTEXT Xusb,
    _In_ WDFREQUEST Request,
    _In_opt_ WDFFILEOBJECT File)
{
    COROOM_VHID_XUSB_ID id;
    PVOID input;
    size_t inputLength;
    BOOLEAN started;
    NTSTATUS status;

    if (File == NULL) {
        return STATUS_ACCESS_DENIED;
    }
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(id), &input, &inputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (inputLength != sizeof(id)) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlCopyMemory(&id, input, sizeof(id));

    (VOID)WdfWaitLockAcquire(Xusb->Lock, NULL);
    if (Xusb->Owner != NULL && Xusb->Owner != File) {
        WdfWaitLockRelease(Xusb->Lock);
        return STATUS_DEVICE_BUSY;
    }
    Xusb->Owner = File;
    Xusb->VendorId = id.VendorId != 0 ? id.VendorId : COROOM_VHID_XBOX360_VENDOR_ID;
    Xusb->ProductId = id.ProductId != 0 ? id.ProductId : COROOM_VHID_XBOX360_PRODUCT_ID;
    RtlZeroMemory(&Xusb->State, sizeof(Xusb->State));
    Xusb->PacketNumber++;
    Xusb->LastStateTick = GetTickCount64();
    Xusb->Resting = FALSE;
    started = Xusb->Started;
    WdfWaitLockRelease(Xusb->Lock);

    if (started) {
        WdfDeviceSetDeviceInterfaceState(Xusb->Device, &GUID_DEVINTERFACE_COROOM_XUSB, NULL,
                                         TRUE);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS
CoroomXusbSetState(
    _In_ PCOROOM_XUSB_CONTEXT Xusb,
    _In_ WDFREQUEST Request,
    _In_opt_ WDFFILEOBJECT File)
{
    COROOM_VHID_XUSB_STATE state;
    PVOID input;
    size_t inputLength;
    BOOLEAN changed;
    NTSTATUS status;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(state), &input, &inputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (inputLength != sizeof(state)) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlCopyMemory(&state, input, sizeof(state));

    (VOID)WdfWaitLockAcquire(Xusb->Lock, NULL);
    if (File == NULL || Xusb->Owner != File) {
        WdfWaitLockRelease(Xusb->Lock);
        return STATUS_ACCESS_DENIED;
    }
    changed = (BOOLEAN)!RtlEqualMemory(&Xusb->State, &state, sizeof(state));
    if (changed || Xusb->Resting) {
        RtlCopyMemory(&Xusb->State, &state, sizeof(state));
        Xusb->PacketNumber++;
    }
    Xusb->LastStateTick = GetTickCount64();
    Xusb->Resting = FALSE;
    WdfWaitLockRelease(Xusb->Lock);

    if (changed) {
        /* Windows.Gaming.Input gets it now instead of on the next pump tick. */
        CoroomXusbPumpOnce(Xusb);
    }
    return STATUS_SUCCESS;
}

/* The xusb22 requests of xinput1_4.dll and Windows.Gaming.Input. */
static VOID
CoroomXusbStandard(
    _In_ PCOROOM_XUSB_CONTEXT Xusb,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ ULONG IoControlCode)
{
    UCHAR buffer[64];
    USHORT vendor;
    USHORT product;
    size_t length = 0;
    NTSTATUS status = STATUS_SUCCESS;

    (VOID)WdfWaitLockAcquire(Xusb->Lock, NULL);
    vendor = Xusb->VendorId;
    product = Xusb->ProductId;
    WdfWaitLockRelease(Xusb->Lock);
    RtlZeroMemory(buffer, sizeof(buffer));

    switch (IoControlCode) {
    case IOCTL_XUSB_GET_INFORMATION:
        /* Version, one controller on this device, VID and PID. */
        CoroomWriteU16(buffer, COROOM_XUSB_VERSION);
        buffer[2] = 0x01;
        CoroomWriteU16(buffer + 8, vendor);
        CoroomWriteU16(buffer + 10, product);
        length = 12;
        break;
    case IOCTL_XUSB_GET_INFORMATION_EX:
        CoroomWriteU16(buffer, COROOM_XUSB_VERSION);
        buffer[2] = 0x01;
        buffer[3] = 0x01;
        CoroomWriteU16(buffer + 8, vendor);
        CoroomWriteU16(buffer + 10, product);
        length = OutputBufferLength < sizeof(buffer) ? OutputBufferLength : sizeof(buffer);
        break;
    case IOCTL_XUSB_GET_CAPABILITIES: {
        /* The two forms a wired Xbox 360 controller answers (HIDMaestro): 24 bytes, or 36 when
           the caller's buffer has room (a 16-byte header with VID, PID and index, then the same
           capabilities): all buttons, full-range triggers and sticks, both vibration motors. */
        static const UCHAR capsV1[24] = {
            0x03, 0x01, 0x00, 0x01, 0xFF, 0xF7, 0xFF, 0xFF,
            0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF,
        };
        static const UCHAR capsV2Tail[20] = {
            0xFF, 0xF7, 0xFF, 0xFF,
            0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF, 0xC0, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF,
        };
        if (OutputBufferLength >= 36) {
            buffer[0] = 0x03;
            buffer[1] = 0x01;
            buffer[2] = 0x01;
            buffer[3] = 0x01;
            buffer[4] = 0x0C;
            buffer[5] = 0x00;
            CoroomWriteU16(buffer + 6, vendor);
            CoroomWriteU16(buffer + 8, product);
            buffer[10] = 0x10;
            buffer[11] = 0x01;
            buffer[12] = Xusb->Slot;
            buffer[13] = 0xFA;
            buffer[14] = 0x34;
            buffer[15] = 0x22;
            RtlCopyMemory(buffer + 16, capsV2Tail, sizeof(capsV2Tail));
            length = 36;
        } else {
            RtlCopyMemory(buffer, capsV1, sizeof(capsV1));
            length = 24;
        }
        break;
    }
    case IOCTL_XUSB_GET_STATE:
        CoroomXusbBuildState(Xusb, FALSE, buffer);
        length = COROOM_XUSB_STATE_SIZE;
        break;
    case IOCTL_XUSB_GET_LED_STATE:
        /* Player LED of its slot (6: player 1 on). */
        buffer[2] = (UCHAR)(0x06 + Xusb->Slot);
        length = 3;
        break;
    case IOCTL_XUSB_GET_BATTERY_INFO:
        /* Wired, full. */
        buffer[2] = 0x01;
        buffer[3] = 0x03;
        length = 4;
        break;
    case IOCTL_XUSB_SET_STATE:
    case IOCTL_XUSB_POWER_INFO:
        /* Vibration is not forwarded to the guest's controller yet: accepted and ignored. */
        length = 0;
        break;
    case IOCTL_XUSB_WAIT_GUIDE:
        /* No guide notification: xinput1_4.dll falls back to polling. */
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }
    if (NT_SUCCESS(status) && length > 0) {
        status = CoroomCompleteWithBytes(Request, buffer, length);
    }
    WdfRequestComplete(Request, status);
}

VOID
CoroomEvtXusbIoctl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    PCOROOM_XUSB_CONTEXT xusb = CoroomGetDeviceContext(WdfIoQueueGetDevice(Queue))->Xusb;
    WDFFILEOBJECT file = WdfRequestGetFileObject(Request);
    COROOM_VHID_INFO info;
    BOOLEAN active;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case COROOM_VHID_IOCTL_GET_INFO:
        RtlZeroMemory(&info, sizeof(info));
        info.ProtocolVersion = COROOM_VHID_PROTOCOL_VERSION;
        info.Kind = COROOM_VHID_KIND_XUSB;
        info.Slot = xusb->Slot;
        (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
        info.Active = (UCHAR)(xusb->Owner != NULL ? 1 : 0);
        WdfWaitLockRelease(xusb->Lock);
        status = CoroomCompleteWithBytes(Request, &info, sizeof(info));
        break;
    case COROOM_VHID_IOCTL_XUSB_ACTIVATE:
        status = CoroomXusbActivate(xusb, Request, file);
        break;
    case COROOM_VHID_IOCTL_XUSB_DEACTIVATE:
        (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
        active = (BOOLEAN)(file != NULL && xusb->Owner == file);
        WdfWaitLockRelease(xusb->Lock);
        if (active) {
            CoroomXusbDeactivate(xusb);
            status = STATUS_SUCCESS;
        } else {
            status = STATUS_ACCESS_DENIED;
        }
        break;
    case COROOM_VHID_IOCTL_XUSB_STATE:
        status = CoroomXusbSetState(xusb, Request, file);
        break;
    default:
        (VOID)WdfWaitLockAcquire(xusb->Lock, NULL);
        active = (BOOLEAN)(xusb->Owner != NULL);
        WdfWaitLockRelease(xusb->Lock);
        if (!active) {
            status = STATUS_DEVICE_NOT_CONNECTED;
            break;
        }
        if (IoControlCode == IOCTL_XUSB_WAIT_FOR_INPUT) {
            /* Parked until the next state or pump tick. */
            status = WdfRequestForwardToIoQueue(Request, xusb->WaitQueue);
            if (NT_SUCCESS(status)) {
                return;
            }
            break;
        }
        CoroomXusbStandard(xusb, Request, OutputBufferLength, IoControlCode);
        return;
    }
    WdfRequestComplete(Request, status);
}

/* ------------------------------------------------------------------------------------------- */
/* Handles                                                                                     */
/* ------------------------------------------------------------------------------------------- */

/* The client closed its handle (or died): its gamepads go away, its Xbox 360 slot is freed. */
VOID
CoroomEvtFileCleanup(
    _In_ WDFFILEOBJECT FileObject)
{
    PCOROOM_DEVICE_CONTEXT context = CoroomGetDeviceContext(WdfFileObjectGetDevice(FileObject));
    BOOLEAN owner;

    if (context->Control != NULL) {
        /* The control device is exclusive: this handle owned every gamepad. */
        CoroomDestroyAllPads(context->Control);
        (VOID)WdfWaitLockAcquire(context->Control->Lock, NULL);
        context->Control->OutputHead = 0;
        context->Control->OutputCount = 0;
        WdfWaitLockRelease(context->Control->Lock);
    }
    if (context->Xusb != NULL) {
        (VOID)WdfWaitLockAcquire(context->Xusb->Lock, NULL);
        owner = (BOOLEAN)(context->Xusb->Owner == FileObject);
        WdfWaitLockRelease(context->Xusb->Lock);
        if (owner) {
            CoroomXusbDeactivate(context->Xusb);
        }
    }
}
