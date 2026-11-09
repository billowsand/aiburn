/*
 * mock_is903 - a LD_PRELOAD shim that emulates an IS903 device on top of
 * SG_IO, so that is903burn can be exercised without real hardware.
 *
 * Usage:
 *   LD_PRELOAD=./mock_is903.so IS903_SG=/tmp/mockdev ./is903burn info
 *
 * The shim intercepts ioctl(SG_IO) and answers vendor CDBs:
 *
 *   INQU (group 0)        -> reply with the command tag list, IS903, Innostor
 *   P.M.  read  (op 0x00) -> deterministic pseudo random data
 *   P.M.  write (op 0x03) -> records the data into $MOCK_WRITE_OUT
 *   P.M.  op 0x08        -> 2 byte challenge
 *   P.M.  op 0x07        -> 16 byte status
 *
 * Build: gcc -shared -fPIC -o mock_is903.so mock_is903.c -ldl
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <scsi/sg.h>

#define SG_IO_ 0x2285

static int (*real_ioctl)(int, unsigned long, ...);

static const char *const TAGS = "INQUPINFEXECNANDTEPFNFTLP.M.";

static uint32_t lcg_state;

static uint32_t lcg(void)
{
	lcg_state = lcg_state * 1103515245u + 12345u;
	return (lcg_state >> 16) & 0x7fff;
}

int ioctl(int fd, unsigned long req, ...)
{
	va_list ap;
	void *arg;

	va_start(ap, req);
	arg = va_arg(ap, void *);
	va_end(ap);

	if (!real_ioctl)
		real_ioctl = dlsym(RTLD_NEXT, "ioctl");

	if (req != SG_IO_)
		return real_ioctl(fd, req, arg);

	{
		sg_io_hdr_t *h = arg;
		const unsigned char *cdb = h->cmdp;
		unsigned char *data = h->dxferp;
		int group = cdb[1], op = cdb[2];
		uint32_t off = ((uint32_t)cdb[4] << 24) | ((uint32_t)cdb[5] << 16) |
			       ((uint32_t)cdb[6] << 8) | cdb[7];
		uint16_t cnt = (uint16_t)((cdb[8] << 8) | cdb[9]);

		if (cdb[0] != 0xfc) {
			/* standard SCSI commands */
			if (cdb[0] == 0x25) {		/* READ CAPACITY(10) */
				uint32_t last = 0x3fff;	/* 32 MiB disc */

				if (h->dxfer_len >= 8) {
					data[0] = (last >> 24) & 0xff;
					data[1] = (last >> 16) & 0xff;
					data[2] = (last >> 8) & 0xff;
					data[3] = last & 0xff;
					data[4] = 0;	/* block size hi */
					data[5] = 2;	/* 512 */
					data[6] = 0;
					data[7] = 0;
				}
				h->status = 0;
				h->masked_status = 0;
				h->host_status = 0;
				h->driver_status = 0;
				h->sb_len_wr = 0;
				h->resid = 0;
				return 0;
			}
			/* anything else: report illegal request */
			memset(h->sbp, 0, h->mx_sb_len);
			h->sbp[0] = 0x70;
			h->sbp[2] = 0x05;	/* ILLEGAL REQUEST */
			h->sbp[7] = 0x24;
			h->sbp[12] = 0x24;
			h->sb_len_wr = 14;
			h->masked_status = 0x01;
			h->host_status = 0x01;
			h->driver_status = 0x08;
			return -1;
		}

		/* only clear the reply buffer for reads; writes carry data */
		if (h->dxfer_direction != SG_DXFER_TO_DEV)
			memset(data, 0, h->dxfer_len);
		h->status = 0;
		h->masked_status = 0;
		h->host_status = 0;
		h->driver_status = 0;
		h->sb_len_wr = 0;
		h->resid = 0;

		if (group == 0 && op == 0) {		/* INQU */
			size_t n = h->dxfer_len;

			if (n > 0x20)
				memcpy(data + 0x10, TAGS, strlen(TAGS));
			if (n > 0x60)
				memcpy(data + 0x50, "IS903", 5);
			if (n > 0x70)
				memcpy(data + 0x60, "Innostor", 8);
			if (n > 0x80)
				memcpy(data + 0x74, "1.00", 4);
		} else if (group == 6 && op == 0x00) {	/* P.M. read */
			unsigned i;

			lcg_state = off * 2654435761u + 1;
			for (i = 0; i < h->dxfer_len; i++)
				data[i] = (unsigned char)lcg();
		} else if (group == 6 && op == 0x03) {	/* P.M. write */
			const char *out = getenv("MOCK_WRITE_OUT");

			if (out) {
				FILE *f = fopen(out, "ab");

				if (f) {
					fwrite(data, 1, h->dxfer_len, f);
					fclose(f);
				}
			}
			fprintf(stderr, "[mock] write %u bytes at block %u\n",
				h->dxfer_len, off);
		} else if (group == 6 && op == 0x08) {	/* challenge */
			if (h->dxfer_len >= 2) {
				data[0] = 0x5a;
				data[1] = 0xc3;
			}
		} else if (group == 6 && op == 0x07) {	/* status */
			if (h->dxfer_len >= 16)
				memset(data, 0x5a, 16);
		} else if (group == 6 && op == 0xff) {	/* key */
			fprintf(stderr, "[mock] key response received\n");
		}
		return 0;
	}
}
