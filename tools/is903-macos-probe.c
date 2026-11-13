// IS903 USB Bulk-Only Transport and standalone read-only probe.
#include <libusb.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

enum { interface_number = 0 };
struct pipes { unsigned char in, out; };

static bool bulk(libusb_device_handle *device, unsigned char endpoint,
                 unsigned char *data, int size, int *actual, unsigned int timeout)
{
    int rc = libusb_bulk_transfer(device, endpoint, data, size, actual, timeout);
    if (rc == 0)
        return true;
    fprintf(stderr, "Bulk endpoint 0x%02x: %s\n", endpoint, libusb_error_name(rc));
    return false;
}

static int scsi(libusb_device_handle *device, struct pipes pipes, uint32_t tag,
                const unsigned char *cdb, unsigned char cdb_size,
                unsigned char *data, int size, bool data_in, unsigned int timeout)
{
    unsigned char cbw[31] = {'U', 'S', 'B', 'C'};
    for (unsigned int i = 0; i < 4; ++i) {
        cbw[4 + i] = tag >> (8 * i);
        cbw[8 + i] = (uint32_t)size >> (8 * i);
    }
    cbw[12] = data_in ? 0x80 : 0;
    cbw[14] = cdb_size;
    memcpy(cbw + 15, cdb, cdb_size);
    int actual, transferred;
    if (!bulk(device, pipes.out, cbw, sizeof(cbw), &actual, timeout) || actual != sizeof(cbw))
        return -1;
    if (!bulk(device, data_in ? pipes.in : pipes.out, data, size, &transferred, timeout))
        return -1;

    unsigned char csw[13];
    if (!bulk(device, pipes.in, csw, sizeof(csw), &actual, timeout) || actual != sizeof(csw))
        return -1;
    uint32_t residue = csw[8] | (uint32_t)csw[9] << 8 |
                       (uint32_t)csw[10] << 16 | (uint32_t)csw[11] << 24;
    if (memcmp(csw, "USBS", 4) != 0 || memcmp(csw + 4, cbw + 4, 4) != 0 ||
        residue != (uint32_t)(size - transferred) || csw[12]) {
        fprintf(stderr, "Invalid BOT result for opcode 0x%02x (status=%u, residue=%u, transferred=%d)\n",
                cdb[0], csw[12], residue, transferred);
        return -1;
    }
    return transferred;
}

#ifndef IS903_BACKEND
static int read_scsi(libusb_device_handle *device, struct pipes pipes, uint32_t tag,
                     const unsigned char *cdb, unsigned char cdb_size,
                     unsigned char *data, int size)
{
    return scsi(device, pipes, tag, cdb, cdb_size, data, size, true, 5000);
}

static uint32_t read_be32(const unsigned char *data)
{
    return (uint32_t)data[0] << 24 | (uint32_t)data[1] << 16 |
           (uint32_t)data[2] << 8 | data[3];
}

static int probe(libusb_device_handle *device, struct pipes pipes)
{
    const unsigned char inquiry[6] = {0x12, 0, 0, 0, 36};
    const unsigned char capacity[10] = {0x25};
    const unsigned char inqu[16] = {
        0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 'I', 'N', 'Q', 'U'
    };
    unsigned char data[134];
    if (read_scsi(device, pipes, 1, inquiry, sizeof(inquiry), data, 36) != 36)
        return 1;
    printf("INQUIRY: type=%u vendor=%.8s product=%.16s\n", data[0] & 0x1f, data + 8, data + 16);
    if ((data[0] & 0x1f) != 5)
        return 1;
    if (read_scsi(device, pipes, 2, capacity, sizeof(capacity), data, 8) != 8)
        return 1;
    printf("READ CAPACITY: last LBA=%u, block size=%u\n", read_be32(data), read_be32(data + 4));
    int received = read_scsi(device, pipes, 3, inqu, sizeof(inqu), data, sizeof(data));
    if (received < 0)
        return 1;
    printf("Vendor INQU: %d bytes\n", received);
    for (int i = 0; i <= received - 5; ++i) {
        if (memcmp(data + i, "IS903", 5) == 0) {
            puts("Vendor response confirms IS903");
            return 0;
        }
    }
    fputs("Vendor response did not contain IS903\n", stderr);
    return 1;
}

#endif

static struct pipes find_pipes(libusb_device *device)
{
    struct pipes pipes = {0};
    struct libusb_config_descriptor *config;
    if (libusb_get_active_config_descriptor(device, &config) != 0)
        return pipes;
    for (int i = 0; i < config->bNumInterfaces; ++i) {
        const struct libusb_interface *iface = &config->interface[i];
        for (int j = 0; j < iface->num_altsetting; ++j) {
            const struct libusb_interface_descriptor *alt = &iface->altsetting[j];
            if (alt->bInterfaceNumber != interface_number || alt->bAlternateSetting != 0 ||
                alt->bInterfaceClass != 8 || alt->bInterfaceSubClass != 6 || alt->bInterfaceProtocol != 0x50)
                continue;
            for (int k = 0; k < alt->bNumEndpoints; ++k) {
                const struct libusb_endpoint_descriptor *ep = &alt->endpoint[k];
                if ((ep->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK) != LIBUSB_TRANSFER_TYPE_BULK)
                    continue;
                if (ep->bEndpointAddress & LIBUSB_ENDPOINT_IN)
                    pipes.in = ep->bEndpointAddress;
                else
                    pipes.out = ep->bEndpointAddress;
            }
        }
    }
    libusb_free_config_descriptor(config);
    return pipes;
}

#ifndef IS903_BACKEND
int main(int argc, char **argv)
{
    bool capture = argc == 2 && strcmp(argv[1], "--capture") == 0;
    if (argc != 1 && !capture) {
        fprintf(stderr, "Usage: %s [--capture]\n", argv[0]);
        return 2;
    }
    if (capture && geteuid() != 0) {
        fputs("Unmount the IS903 volumes, then run --capture with sudo.\n", stderr);
        return 1;
    }
    libusb_context *context;
    int rc = libusb_init_context(&context, NULL, 0);
    if (rc != 0) {
        fprintf(stderr, "libusb init: %s\n", libusb_error_name(rc));
        return 1;
    }
    libusb_device **list;
    ssize_t count = libusb_get_device_list(context, &list);
    libusb_device *target = NULL;
    int matches = 0;
    int result = 1;
    for (ssize_t i = 0; i < count; ++i) {
        struct libusb_device_descriptor descriptor;
        if (libusb_get_device_descriptor(list[i], &descriptor) == 0 &&
            descriptor.idVendor == 0x1f75 && descriptor.idProduct == 0x0903) {
            target = list[i];
            ++matches;
        }
    }
    if (count < 0) {
        fprintf(stderr, "USB enumeration: %s\n", libusb_error_name((int)count));
    } else if (matches != 1) {
        fprintf(stderr, "Expected exactly one 1f75:0903 device, found %d\n", matches);
    } else {
        struct pipes pipes = find_pipes(target);
        printf("IS903 BOT endpoints: IN=0x%02x OUT=0x%02x\n", pipes.in, pipes.out);
        libusb_device_handle *device;
        rc = libusb_open(target, &device);
        if (rc != 0) {
            fprintf(stderr, "USB open: %s\n", libusb_error_name(rc));
        } else if (capture && pipes.in && pipes.out) {
            rc = libusb_set_auto_detach_kernel_driver(device, 1);
            if (rc == 0)
                rc = libusb_claim_interface(device, interface_number);
            if (rc != 0) {
                fprintf(stderr, "USB capture/claim: %s\n", libusb_error_name(rc));
            } else {
                result = probe(device, pipes);
                rc = libusb_release_interface(device, interface_number);
                if (rc != 0) {
                    fprintf(stderr, "USB release: %s\n", libusb_error_name(rc));
                    result = 1;
                }
            }
            libusb_close(device);
        } else {
            int active = libusb_kernel_driver_active(device, interface_number);
            printf("Kernel driver: %s\n", active < 0 ? libusb_error_name(active) : active ? "active" : "inactive");
            result = pipes.in && pipes.out ? 0 : 1;
            libusb_close(device);
        }
    }
    if (count >= 0)
        libusb_free_device_list(list, 1);
    libusb_exit(context);
    return result;
}

#else

struct is903_usb {
    libusb_context *context;
    libusb_device_handle *device;
    struct pipes pipes;
    uint32_t tag;
};

int is903_usb_open(unsigned long session, struct is903_usb **output)
{
    struct is903_usb *usb = calloc(1, sizeof(*usb));
    if (!usb)
        return LIBUSB_ERROR_NO_MEM;
    int rc = libusb_init_context(&usb->context, NULL, 0);
    if (rc != 0) {
        free(usb);
        return rc;
    }

    libusb_device **list;
    ssize_t count = libusb_get_device_list(usb->context, &list);
    rc = count < 0 ? (int)count : LIBUSB_ERROR_NO_DEVICE;
    for (ssize_t i = 0; i < count; ++i) {
        struct libusb_device_descriptor descriptor;
        if (libusb_get_session_data(list[i]) != session ||
            libusb_get_device_descriptor(list[i], &descriptor) != 0 ||
            descriptor.idVendor != 0x1f75 || descriptor.idProduct != 0x0903)
            continue;
        usb->pipes = find_pipes(list[i]);
        if (!usb->pipes.in || !usb->pipes.out) {
            rc = LIBUSB_ERROR_NOT_SUPPORTED;
            break;
        }
        rc = libusb_open(list[i], &usb->device);
        if (rc == 0)
            rc = libusb_set_auto_detach_kernel_driver(usb->device, 1);
        if (rc == 0)
            rc = libusb_claim_interface(usb->device, interface_number);
        break;
    }
    if (count >= 0)
        libusb_free_device_list(list, 1);
    if (rc == 0) {
        *output = usb;
    } else {
        if (usb->device)
            libusb_close(usb->device);
        libusb_exit(usb->context);
        free(usb);
    }
    return rc;
}

int is903_usb_close(struct is903_usb *usb)
{
    int rc = libusb_release_interface(usb->device, interface_number);
    libusb_close(usb->device);
    libusb_exit(usb->context);
    free(usb);
    return rc;
}

int is903_usb_cmd(struct is903_usb *usb, const unsigned char *cdb,
                  unsigned char cdb_size, unsigned char *data, int size,
                  int data_in, unsigned int timeout)
{
    return scsi(usb->device, usb->pipes, ++usb->tag, cdb, cdb_size, data, size,
                data_in != 0, timeout);
}

#endif
