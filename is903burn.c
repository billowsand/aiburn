/*
 * is903burn - native Linux replacement for Innostar AI-Burn (AIBurn.exe)
 *
 * AI-Burn.exe is a 32-bit Delphi GUI that talks to the Innostar IS903
 * "UFCD" (USB Flash CD-ROM) chip through Windows SCSI pass-through and a
 * set of vendor-specific SCSI commands (opcode 0xFC).
 *
 * This program reproduces that protocol on Linux using SG_IO on /dev/sgN.
 *
 * Vendor CDB layout (16 bytes), reverse engineered from AIBurn.exe:
 *
 *   [0]     0xFC                vendor opcode
 *   [1]     command group       0=INQU 1=PINF 2=EXEC 3=NAND 4=TEPF
 *                               5=NFTL 6=P.M.
 *   [2]     sub opcode          per group (see IS903_* below)
 *   [3]     parameter
 *   [4..7]  offset (big endian) LBA / byte offset
 *   [8..9]  count  (big endian) number of 512-byte blocks
 *   [10]    flag
 *   [11]    reserved (0)
 *   [12..15] ASCII tag of the command group ("INQU", "P.M.", ...)
 *
 * The device advertises the list of supported groups in the INQU reply
 * as the concatenated 4-character tags; the index of a tag in that list
 * is the value used in CDB byte 1.
 *
 * This program is free software; distributed under the terms of the
 * GNU General Public License version 2 or later.
 */

#define _GNU_SOURCE
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <scsi/sg.h>

/* ------------------------------------------------------------------ */
/* IS903 vendor command definitions                                    */
/* ------------------------------------------------------------------ */

/* command groups. The index of a group is the value that goes into
 * CDB byte 1, and the device rejects a command whose ASCII tag (CDB
 * bytes 12-15) does not match the group at that index. */
enum {
	GRP_INQU = 0,
	GRP_PINF = 1,
	GRP_EXEC = 2,
	GRP_NAND = 3,
	GRP_TEPF = 4,
	GRP_NFTL = 5,
	GRP_PM   = 6,		/* "P.M." - physical media (the ISO image) */
	GRP_MAX
};

static const char *const grp_tag[GRP_MAX] = {
	"INQU", "PINF", "EXEC", "NAND", "TEPF", "NFTL", "P.M."
};

/* Tags as advertised by the connected device, indexed by group. Filled
 * in by discover_groups(). The defaults above are only a fallback. */
static char cur_tag[GRP_MAX][8] = {
	"INQU", "PINF", "EXEC", "NAND", "TEPF", "NFTL", "P.M."
};

/* Group index of the "P.M." (physical media) command on this device. */
static int pm_group = GRP_PM;

/* sub opcodes of the "P.M." group, as used by AIBurn.exe */
#define PM_OP_READ	0x00	/* read blocks from the stored image   */
#define PM_OP_WRITE	0x03	/* write blocks into the stored image   */
#define PM_OP_STATUS	0x07	/* status query                        */
#define PM_OP_INFO	0x08	/* 2 byte challenge / info             */
#define PM_OP_KEY	0xff	/* 12 byte key response                */

/* AIBurn.exe always uses this value for CDB byte 3 of the P.M. read and
 * write commands. */
#define PM_PARAM	0x01

/* AIBurn.exe transfers at most 0x80 blocks (64 KiB) per command and
 * advances the block offset by the same amount between commands. */
#define PM_CHUNK_BLOCKS	0x80

#define VENDOR_OPCODE	0xfc

/* ------------------------------------------------------------------ */
/* small helpers                                                       */
/* ------------------------------------------------------------------ */

static const char *progname = "is903burn";

static void die(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	fprintf(stderr, "%s: ", progname);
	vfprintf(stderr, fmt, ap);
	fputc('\n', stderr);
	va_end(ap);
	exit(1);
}

static void *xmalloc(size_t n)
{
	void *p = malloc(n);

	if (!p)
		die("out of memory");
	return p;
}

static void hexdump(const char *label, const void *buf, size_t len)
{
	const unsigned char *p = buf;
	size_t i;

	printf("%s (%zu bytes):\n", label, len);
	for (i = 0; i < len; i++) {
		if ((i % 16) == 0)
			printf("  %04zx  ", i);
		printf("%02x ", p[i]);
		if ((i % 16) == 15 || i + 1 == len)
			putchar('\n');
	}
}

/* ------------------------------------------------------------------ */
/* device discovery                                                    */
/* ------------------------------------------------------------------ */

struct dev_info {
	char sg_path[300];	/* /dev/sgN             */
	char sr_path[300];	/* /dev/srN (if found)  */
	char vendor[64];
	char product[128];
	char rev[16];
	int  type;		/* SCSI device type     */
	int  index;		/* index in the list    */
};

static bool read_sysfs(const char *dir, const char *name, char *out, size_t outlen)
{
	char path[512];
	FILE *f;
	size_t n;

	snprintf(path, sizeof(path), "%s/%s", dir, name);
	f = fopen(path, "r");
	if (!f)
		return false;
	n = fread(out, 1, outlen - 1, f);
	fclose(f);
	out[n] = '\0';
	while (n && (out[n - 1] == '\n' || out[n - 1] == '\r'))
		out[--n] = '\0';
	return true;
}

/* find the /dev/srN node that belongs to the same SCSI target as the
 * given /dev/sgN node. */
/*
 * Find the block device (/dev/srN or /dev/sdN) that belongs to the same
 * SCSI target as the given /dev/sgN node.
 *
 * The IS903 shows up in one of two ways:
 *   CD-ROM (SCSI type 5):     /dev/srN, read only
 *   mass storage (type 0):    /dev/sdN, writable, ISO image at offset 0
 * Both are found here; the caller decides what to do with the result.
 */
static void find_sr_for_sg(const char *sg_name, char *out, size_t outlen)
{
	DIR *d;
	struct dirent *de;

	out[0] = '\0';
	d = opendir("/sys/block");
	if (!d)
		return;
	while ((de = readdir(d))) {
		char link[512], target[512], sglink[512];
		char srtarget[512];
		ssize_t n;

		if (strncmp(de->d_name, "sr", 2) != 0 &&
		    strncmp(de->d_name, "sd", 2) != 0)
			continue;
		snprintf(link, sizeof(link), "/sys/block/%s/device", de->d_name);
		n = readlink(link, target, sizeof(target) - 1);
		if (n <= 0)
			continue;
		target[n] = '\0';
		snprintf(sglink, sizeof(sglink), "/sys/class/scsi_generic/%s/device", sg_name);
		n = readlink(sglink, srtarget, sizeof(srtarget) - 1);
		if (n <= 0)
			continue;
		srtarget[n] = '\0';
		/* strip the trailing "/block/srN" from the sr target */
		{
			char *slash = strstr(target, "/block/");
			if (slash)
				*slash = '\0';
		}
		if (strcmp(target, srtarget) == 0) {
			snprintf(out, outlen, "/dev/%s", de->d_name);
			break;
		}
	}
	closedir(d);
}

static struct dev_info *discover(int *count)
{
	struct dev_info *list = NULL;
	DIR *d;
	struct dirent *de;
	const char *override;
	int n = 0;

	/* Allow an explicit device, mostly for testing. */
	override = getenv("IS903_SG");
	if (override && *override) {
		const char *blk = getenv("IS903_BLK");

		list = xmalloc(sizeof(*list));
		memset(list, 0, sizeof(*list));
		snprintf(list[0].sg_path, sizeof(list[0].sg_path), "%s",
			 override);
		snprintf(list[0].vendor, sizeof(list[0].vendor), "Innostor");
		snprintf(list[0].product, sizeof(list[0].product), "CD-ROM on Flash");
		list[0].type = 5;
		/* a mass-storage style setup for the burn/dump tests */
		if (blk && *blk) {
			snprintf(list[0].sr_path, sizeof(list[0].sr_path), "%s", blk);
			list[0].type = 0;
		}
		list[0].index = 0;
		*count = 1;
		return list;
	}

	d = opendir("/sys/class/scsi_generic");
	if (!d)
		die("cannot open /sys/class/scsi_generic: %s", strerror(errno));
	while ((de = readdir(d))) {
		char dir[512];
		struct dev_info di;

		if (strncmp(de->d_name, "sg", 2) != 0 || !isdigit((unsigned char)de->d_name[2]))
			continue;
		snprintf(dir, sizeof(dir), "/sys/class/scsi_generic/%s/device", de->d_name);
		memset(&di, 0, sizeof(di));
		read_sysfs(dir, "vendor", di.vendor, sizeof(di.vendor));
		read_sysfs(dir, "model", di.product, sizeof(di.product));
		read_sysfs(dir, "rev", di.rev, sizeof(di.rev));
		{
			char t[32];
			if (read_sysfs(dir, "type", t, sizeof(t)))
				di.type = atoi(t);
		}
		snprintf(di.sg_path, sizeof(di.sg_path), "/dev/%s", de->d_name);
		find_sr_for_sg(de->d_name, di.sr_path, sizeof(di.sr_path));
		di.index = n;
		list = realloc(list, (n + 1) * sizeof(*list));
		if (!list)
			die("out of memory");
		list[n] = di;
		n++;
	}
	closedir(d);
	*count = n;
	return list;
}

static bool dev_is_is903(const struct dev_info *di)
{
	/* The device reports "Innostor" as its SCSI vendor string. */
	if (strstr(di->vendor, "Innosto"))
		return true;
	if (strstr(di->product, "Flash"))
		return true;
	return false;
}

/* ------------------------------------------------------------------ */
/* SG_IO transport                                                     */
/* ------------------------------------------------------------------ */

struct is903 {
	int fd;
	char path[300];
};

static int is903_open(struct is903 *d, const char *path)
{
	d->fd = open(path, O_RDWR);
	if (d->fd < 0) {
		if (errno == EACCES)
			die("permission denied opening %s "
			    "(try: sudo modprobe sg)", path);
		die("cannot open %s: %s", path, strerror(errno));
	}
	snprintf(d->path, sizeof(d->path), "%s", path);
	return 0;
}

static void is903_close(struct is903 *d)
{
	if (d->fd >= 0)
		close(d->fd);
	d->fd = -1;
}

/*
 * Send a vendor command. dir is SG_DXFER_NONE, SG_DXFER_FROM_DEV or
 * SG_DXFER_TO_DEV.
 *
 * "count" goes into CDB bytes 8-9 and "len" is the data transfer length.
 * For the block oriented P.M. read/write both are the same (blocks and
 * blocks*512). For the small status/info/key commands the count field
 * carries the transfer length in bytes, exactly as AIBurn.exe does.
 *
 * Returns 0 on success, -1 on failure (sense data is printed).
 */
static int vcmd(struct is903 *d, int group, int op, int param,
		uint32_t offset, uint16_t count, int flag,
		int dir, void *buf, size_t len)
{
	unsigned char cdb[16];
	unsigned char sense[32];
	sg_io_hdr_t hdr;
	size_t taglen;
	int rc;

	memset(cdb, 0, sizeof(cdb));
	taglen = strlen(cur_tag[group]);
	if (taglen > 4)
		taglen = 4;
	memcpy(cdb + 12, cur_tag[group], taglen);

	cdb[0] = VENDOR_OPCODE;
	cdb[1] = (unsigned char)group;
	cdb[2] = (unsigned char)op;
	cdb[3] = (unsigned char)param;
	cdb[4] = (unsigned char)(offset >> 24);
	cdb[5] = (unsigned char)(offset >> 16);
	cdb[6] = (unsigned char)(offset >> 8);
	cdb[7] = (unsigned char)(offset);
	cdb[8] = (unsigned char)(count >> 8);
	cdb[9] = (unsigned char)(count);
	cdb[10] = (unsigned char)flag;

	memset(sense, 0, sizeof(sense));
	memset(&hdr, 0, sizeof(hdr));
	hdr.interface_id = 'S';
	hdr.cmd_len = sizeof(cdb);
	hdr.mx_sb_len = sizeof(sense);
	hdr.dxfer_direction = dir;
	hdr.dxfer_len = (unsigned int)len;
	hdr.dxferp = buf;
	hdr.cmdp = cdb;
	hdr.sbp = sense;
	hdr.timeout = 30000;

	rc = ioctl(d->fd, SG_IO, &hdr);
	if (rc < 0) {
		fprintf(stderr, "SG_IO failed: %s\n", strerror(errno));
		return -1;
	}
	if (hdr.sb_len_wr || hdr.masked_status || hdr.host_status || hdr.driver_status) {
		fprintf(stderr, "command failed: status=%d masked=%d host=%d driver=%d\n",
			hdr.status, hdr.masked_status, hdr.host_status, hdr.driver_status);
		if (hdr.sb_len_wr)
			hexdump("sense", sense, hdr.sb_len_wr);
		else
			hexdump("sense", sense, sizeof(sense));
		return -1;
	}
	if (len && hdr.resid)
		fprintf(stderr, "warning: %u bytes short transferred\n", hdr.resid);
	return 0;
}

/* ------------------------------------------------------------------ */
/* key response scrambling (reverse engineered from AIBurn.exe)         */
/* ------------------------------------------------------------------ */

/*
 * AIBurn.exe builds a 12 byte buffer from the 11 ASCII bytes
 * "Vm3RuP42j/ " plus one more byte, then XORs it with a pseudo random
 * stream derived from the 16 bit challenge returned by the device:
 *
 *   state = reverse of the low 15 bits of the challenge
 *   for each output byte:
 *       acc = 0
 *       repeat 8 times:
 *           b = (state ^ (state >> 1)) & 1
 *           acc = (acc >> 1) | (b << 7)
 *           state = ((state >> 1) | (b << 14)) & 0x7fff
 *       buf[i] ^= acc
 */
static const unsigned char key_seed[11] = {
	'V', 'm', '3', 'R', 'u', 'P', '4', '2', 'j', '/', ' '
};

static void scramble_key(uint16_t challenge, unsigned char *buf, size_t len)
{
	unsigned int state = 0;
	size_t i;
	int b;

	/* reverse the low 15 bits of the challenge into the LFSR state */
	for (i = 0; i < 15; i++)
		if ((challenge >> (14 - i)) & 1)
			state |= 1u << i;

	for (i = 0; i < len; i++) {
		unsigned char acc = 0;
		int k;

		for (k = 0; k < 8; k++) {
			b = (int)((state ^ (state >> 1)) & 1);
			acc = (unsigned char)((acc >> 1) | (b << 7));
			state = ((state >> 1) | ((unsigned int)b << 14)) & 0x7fff;
		}
		buf[i] ^= acc;
	}
}

/* ------------------------------------------------------------------ */
/* high level commands                                                 */
/* ------------------------------------------------------------------ */

/*
 * The INQU reply contains the list of command groups the device
 * supports, as a run of 4 character ASCII tags. The index of a tag in
 * that list is the value that goes into CDB byte 1.
 *
 * The tags are not necessarily 4 byte aligned inside the reply, so find
 * the longest run of consecutive known tags instead of assuming an
 * alignment.
 */
static int parse_groups(const unsigned char *buf, size_t len, char names[GRP_MAX][8])
{
	size_t best_start = 0, best_len = 0;
	size_t start;

	for (start = 0; start + 4 <= len; start++) {
		size_t n = 0;

		while (start + (n + 1) * 4 <= len) {
			int k, known = 0, dup = 0;

			for (k = 0; k < GRP_MAX; k++)
				if (!memcmp(buf + start + n * 4, grp_tag[k], 4))
					known = 1;
			if (!known)
				break;
			/* a repeated tag means the table ended and other
			 * tag-like strings follow in the reply */
			for (k = 0; k < (int)n; k++)
				if (!memcmp(buf + start + n * 4, buf + start + k * 4, 4))
					dup = 1;
			if (dup)
				break;
			n++;
		}
		/* The group table starts with "INQU". The reply also contains
		 * other tag-like strings further on, so anchor on the start
		 * of the table instead of taking the longest run. */
		if (!memcmp(buf + start, grp_tag[GRP_INQU], 4)) {
			size_t k;

			for (k = 0; k < n; k++) {
				memcpy(names[k], buf + start + k * 4, 4);
				names[k][4] = '\0';
			}
			return (int)n;
		}
		if (n > best_len) {
			best_len = n;
			best_start = start;
		}
	}
	if (best_len == 0)
		return 0;
	for (size_t i = 0; i < best_len && i < GRP_MAX; i++) {
		memcpy(names[i], buf + best_start + i * 4, 4);
		names[i][4] = '\0';
	}
	return (int)best_len;
}

/* Find the index of a given tag in the list the device advertised. */
static int group_index(const char names[GRP_MAX][8], int n, const char *tag)
{
	int i;

	for (i = 0; i < n; i++)
		if (!memcmp(names[i], tag, 4))
			return i;
	return -1;
}

static int cmd_inqu(struct is903 *d)
{
	unsigned char buf[512];
	char names[GRP_MAX][8];
	int n, i;

	memset(buf, 0, sizeof(buf));
	/* AIBurn.exe uses count 0x0080 and reads 0x86 bytes of reply. */
	if (vcmd(d, GRP_INQU, 0x00, 0x00, 0, 0x0080, 0,
		 SG_DXFER_FROM_DEV, buf, 0x86) < 0)
		return -1;
	hexdump("INQU reply", buf, 0x86);
	n = parse_groups(buf, 0x86, names);
	printf("\nsupported command groups:\n");
	if (n == 0) {
		printf("  (none recognised)\n");
		return 0;
	}
	for (i = 0; i < n; i++)
		printf("  %d  %-4s\n", i, names[i]);
	return 0;
}

/*
 * Ask the device which command groups it supports and record the
 * mapping. The device rejects commands whose ASCII tag does not match
 * the group index, so getting this right is essential.
 *
 * Returns 0 on success.
 */
static int discover_groups(struct is903 *d, bool verbose)
{
	unsigned char buf[512];
	char names[GRP_MAX][8];
	int n, i, pm = -1;

	memset(buf, 0, sizeof(buf));
	if (vcmd(d, GRP_INQU, 0x00, 0x00, 0, 0x0080, 0,
		 SG_DXFER_FROM_DEV, buf, 0x86) < 0)
		return -1;
	n = parse_groups(buf, 0x86, names);
	if (n <= 0) {
		if (verbose)
			fprintf(stderr, "%s: could not parse the command group "
				"list from the INQU reply\n", progname);
		return -1;
	}
	for (i = 0; i < n && i < GRP_MAX; i++)
		memcpy(cur_tag[i], names[i], 5);	/* 4 chars + NUL */
	/* a group may exist that this build does not know about; ignore it */
	while (n > 0 && !cur_tag[n - 1][0])
		n--;
	/* locate the physical media group */
	pm = group_index(cur_tag, n, "P.M.");
	if (pm < 0) {
		if (verbose)
			fprintf(stderr, "%s: this device does not offer the "
				"P.M. (physical media) command group\n", progname);
		return -1;
	}
	pm_group = pm;
	if (verbose) {
		printf("supported command groups:\n");
		for (i = 0; i < n; i++)
			printf("  %d  %-4s\n", i, cur_tag[i]);
		printf("\nusing group %d (%s) for physical media access\n",
		       pm_group, cur_tag[pm_group]);
	}
	return 0;
}

static int cmd_status(struct is903 *d)
{
	unsigned char buf[512];

	/* op 0x07: 16 bytes of status, no count field */
	memset(buf, 0, sizeof(buf));
	if (vcmd(d, pm_group, PM_OP_STATUS, 0x00, 0, 0x0000, 0,
		 SG_DXFER_FROM_DEV, buf, 16) == 0) {
		printf("P.M. op 0x07 (status) reply:\n");
		hexdump("  reply", buf, 16);
	} else {
		printf("P.M. op 0x07 (status): failed\n");
	}

	/* op 0x08: 2 byte challenge */
	memset(buf, 0, sizeof(buf));
	if (vcmd(d, pm_group, PM_OP_INFO, 0x00, 0, 0x0002, 0,
		 SG_DXFER_FROM_DEV, buf, 2) == 0) {
		printf("P.M. op 0x08 (challenge) reply:\n");
		hexdump("  reply", buf, 2);
	} else {
		printf("P.M. op 0x08 (challenge): failed\n");
	}
	return 0;
}

/*
 * Send an arbitrary vendor CDB, given as a hex string. Used to explore
 * parts of the protocol that AIBurn.exe does not exercise.
 */
static int cmd_raw(struct is903 *d, const char *hex, int dir, uint32_t len)
{
	unsigned char cdb[16];
	unsigned char buf[65536];
	unsigned char sense[32];
	size_t n = strlen(hex) / 2;
	size_t i;
	int rc;

	if (n > 16)
		die("CDB may be at most 16 bytes");
	for (i = 0; i < n; i++) {
		unsigned v;

		if (sscanf(hex + 2 * i, "%2x", &v) != 1)
			die("bad hex string");
		cdb[i] = (unsigned char)v;
	}
	memset(buf, 0, sizeof(buf));
	memset(sense, 0, sizeof(sense));

	{
		sg_io_hdr_t hdr;

		memset(&hdr, 0, sizeof(hdr));
		hdr.interface_id = 'S';
		hdr.cmd_len = n;
		hdr.mx_sb_len = sizeof(sense);
		hdr.dxfer_direction = dir;
		hdr.dxfer_len = len;
		hdr.dxferp = buf;
		hdr.cmdp = cdb;
		hdr.sbp = sense;
		hdr.timeout = 30000;
		rc = ioctl(d->fd, SG_IO, &hdr);
		if (rc < 0) {
			fprintf(stderr, "SG_IO failed: %s\n", strerror(errno));
			return -1;
		}
		printf("status=%d masked=%d host=%d driver=%d resid=%u\n",
		       hdr.status, hdr.masked_status, hdr.host_status,
		       hdr.driver_status, hdr.resid);
		if (hdr.sb_len_wr)
			hexdump("sense", sense, hdr.sb_len_wr);
		if (len)
			hexdump("data", buf, len);
	}
	return 0;
}

/* ------------------------------------------------------------------ */
/* direct block-device access                                          */
/* ------------------------------------------------------------------ */

/*
 * In mass-storage mode the ISO image lives verbatim at offset 0 of the
 * block device, so burning is a plain write and dumping is a plain read.
 * This path is the one that has been verified on real hardware.
 */

#define ISO_SECTOR	2048u	/* ISO 9660 logical sector size   */
#define ISO_PVD_LBA	16u	/* primary volume descriptor      */

/* Read the volume space size (in ISO sectors) from the PVD. */
static int iso_volume_blocks(int fd, uint32_t *blocks)
{
	unsigned char pvd[ISO_SECTOR];
	ssize_t n;

	n = pread(fd, pvd, sizeof(pvd), (off_t)ISO_PVD_LBA * ISO_SECTOR);
	if (n != (ssize_t)sizeof(pvd))
		return -1;
	if (memcmp(pvd + 1, "CD001", 5) != 0)
		return -1;
	*blocks = (uint32_t)pvd[80] | ((uint32_t)pvd[81] << 8) |
		  ((uint32_t)pvd[82] << 16) | ((uint32_t)pvd[83] << 24);
	return 0;
}

static int blk_read_iso(const char *path, const char *out)
{
	unsigned char *buf;
	uint32_t blocks, done = 0;
	int fd;
	FILE *f;

	fd = open(path, O_RDONLY);
	if (fd < 0)
		die("cannot open %s: %s", path, strerror(errno));
	if (iso_volume_blocks(fd, &blocks) < 0) {
		close(fd);
		fprintf(stderr, "%s: no ISO 9660 filesystem found at offset 0\n",
			path);
		return -1;
	}
	printf("ISO volume: %u sectors of %u bytes (%llu bytes)\n",
	       blocks, ISO_SECTOR, (unsigned long long)blocks * ISO_SECTOR);

	buf = xmalloc(ISO_SECTOR * 128u);
	f = fopen(out, "wb");
	if (!f)
		die("cannot create %s: %s", out, strerror(errno));
	while (done < blocks) {
		uint32_t n = blocks - done;
		ssize_t got;

		if (n > 128)
			n = 128;
		got = pread(fd, buf, (size_t)n * ISO_SECTOR,
			    (off_t)done * ISO_SECTOR);
		if (got != (ssize_t)((size_t)n * ISO_SECTOR)) {
			fprintf(stderr, "short read at sector %u\n", done);
			fclose(f);
			free(buf);
			close(fd);
			return -1;
		}
		if (fwrite(buf, 1, (size_t)got, f) != (size_t)got) {
			fclose(f);
			free(buf);
			close(fd);
			die("short write to %s", out);
		}
		done += n;
	}
	fclose(f);
	free(buf);
	close(fd);
	printf("wrote %s\n", out);
	return 0;
}

static int blk_write_iso(const char *path, const char *in)
{
	unsigned char *buf;
	FILE *f;
	long size;
	int fd;
	uint32_t written = 0;

	f = fopen(in, "rb");
	if (!f)
		die("cannot open %s: %s", in, strerror(errno));
	if (fseek(f, 0, SEEK_END) != 0)
		die("cannot seek %s", in);
	size = ftell(f);
	rewind(f);
	if (size < 0)
		die("cannot determine the size of %s", in);
	if (size % ISO_SECTOR)
		fprintf(stderr, "warning: %s is not a multiple of %u bytes; "
			"padding the last sector\n", in, ISO_SECTOR);

	fd = open(path, O_RDWR);
	if (fd < 0)
		die("cannot open %s: %s", path, strerror(errno));

	buf = xmalloc(ISO_SECTOR * 128u);
	for (;;) {
		size_t got = fread(buf, 1, ISO_SECTOR * 128u, f);
		ssize_t n;

		if (got == 0)
			break;
		if (got % ISO_SECTOR) {
			size_t pad = ISO_SECTOR - (got % ISO_SECTOR);

			memset(buf + got, 0, pad);
			got += pad;
		}
		n = pwrite(fd, buf, got, (off_t)written * ISO_SECTOR);
		if (n != (ssize_t)got) {
			fprintf(stderr, "short write at sector %u: %s\n",
				written, strerror(errno));
			fclose(f);
			free(buf);
			close(fd);
			return -1;
		}
		written += (uint32_t)(got / ISO_SECTOR);
	}
	free(buf);
	fclose(f);
	if (fsync(fd) != 0)
		fprintf(stderr, "warning: fsync failed: %s\n", strerror(errno));
	close(fd);
	printf("wrote %ld bytes (%u ISO sectors) to %s\n",
	       size, written, path);
	return 0;
}

static int cmd_read(struct is903 *d, uint32_t lba, uint32_t blocks, const char *out)
{
	unsigned char *buf;
	FILE *f;
	uint32_t done = 0;

	buf = xmalloc((size_t)PM_CHUNK_BLOCKS * 512);
	f = fopen(out, "wb");
	if (!f)
		die("cannot create %s: %s", out, strerror(errno));
	while (done < blocks) {
		uint32_t n = blocks - done;
		size_t got;

		if (n > PM_CHUNK_BLOCKS)
			n = PM_CHUNK_BLOCKS;
		got = (size_t)n * 512;
		if (vcmd(d, pm_group, PM_OP_READ, PM_PARAM, lba + done, (uint16_t)n, 0,
			 SG_DXFER_FROM_DEV, buf, got) < 0) {
			fclose(f);
			free(buf);
			return -1;
		}
		if (fwrite(buf, 1, got, f) != got) {
			fclose(f);
			free(buf);
			die("short write to %s", out);
		}
		done += n;
	}
	fclose(f);
	free(buf);
	printf("read %u blocks from LBA %u into %s\n", blocks, lba, out);
	return 0;
}

static int cmd_write(struct is903 *d, uint32_t lba, const char *in)
{
	unsigned char *buf;
	FILE *f;
	long size;
	uint32_t lba_cur = lba;

	f = fopen(in, "rb");
	if (!f)
		die("cannot open %s: %s", in, strerror(errno));
	if (fseek(f, 0, SEEK_END) != 0)
		die("cannot seek %s", in);
	size = ftell(f);
	rewind(f);
	if (size < 0)
		die("cannot determine size of %s", in);

	/* AIBurn.exe transfers 64 KiB (0x80 blocks) per command. The
	 * buffer is zeroed once so a short final chunk does not send
	 * stale bytes. */
	buf = xmalloc(0x10000);
	memset(buf, 0, 0x10000);
	for (;;) {
		size_t got = fread(buf, 1, 0x10000, f);
		uint32_t blocks;

		if (got == 0)
			break;
		/* the tail of a short chunk must not carry stale data */
		if (got < 0x10000)
			memset(buf + got, 0, 0x10000 - got);
		blocks = (uint32_t)((got + 511) / 512);
		if (vcmd(d, pm_group, PM_OP_WRITE, PM_PARAM, lba_cur, (uint16_t)blocks, 0,
			 SG_DXFER_TO_DEV, buf, (size_t)blocks * 512) < 0) {
			fclose(f);
			free(buf);
			return -1;
		}
		lba_cur += blocks;
	}
	fclose(f);
	free(buf);
	printf("wrote %ld bytes starting at LBA %u\n", size, lba);
	return 0;
}

/*
 * Standard SCSI READ CAPACITY. Safe, and tells us how big the virtual
 * disc is.
 */
static int cmd_capacity(struct is903 *d)
{
	unsigned char cdb[10];
	unsigned char buf[8];
	unsigned char sense[32];
	sg_io_hdr_t hdr;
	uint32_t last;

	memset(cdb, 0, sizeof(cdb));
	cdb[0] = 0x25;		/* READ CAPACITY(10) */
	memset(buf, 0, sizeof(buf));
	memset(sense, 0, sizeof(sense));

	memset(&hdr, 0, sizeof(hdr));
	hdr.interface_id = 'S';
	hdr.cmd_len = sizeof(cdb);
	hdr.mx_sb_len = sizeof(sense);
	hdr.dxfer_direction = SG_DXFER_FROM_DEV;
	hdr.dxfer_len = sizeof(buf);
	hdr.dxferp = buf;
	hdr.cmdp = cdb;
	hdr.sbp = sense;
	hdr.timeout = 30000;

	if (ioctl(d->fd, SG_IO, &hdr) < 0) {
		fprintf(stderr, "SG_IO failed: %s\n", strerror(errno));
		return -1;
	}
	if (hdr.masked_status || hdr.host_status || hdr.driver_status) {
		fprintf(stderr, "READ CAPACITY failed (masked=%d host=%d driver=%d)\n",
			hdr.masked_status, hdr.host_status, hdr.driver_status);
		return -1;
	}
	last = ((uint32_t)buf[0] << 24) | ((uint32_t)buf[1] << 16) |
	       ((uint32_t)buf[2] << 8) | buf[3];
	printf("last LBA: %u (capacity %u blocks, %llu bytes)\n", last, last + 1,
	       (unsigned long long)(last + 1) * 512);
	return 0;
}

/*
 * The 12 byte key exchange that AIBurn.exe performs before writing:
 * read a 2 byte challenge with P.M. op 0x08, scramble the built in
 * seed with it and send the result back with P.M. op 0xff.
 *
 * "pcs" is the CDB byte 3 value used for the response. AIBurn.exe
 * takes it from its per-device settings table; the device offers six
 * PCS slots, so it is selectable here.
 */
static int cmd_key(struct is903 *d, int pcs)
{
	unsigned char challenge[2];
	unsigned char key[12];

	memset(challenge, 0, sizeof(challenge));
	if (vcmd(d, pm_group, PM_OP_INFO, 0x00, 0, 0x0002, 0,
		 SG_DXFER_FROM_DEV, challenge, sizeof(challenge)) < 0)
		return -1;
	printf("challenge: %02x %02x\n", challenge[0], challenge[1]);

	memcpy(key, key_seed, sizeof(key_seed));
	key[11] = 0;
	scramble_key((uint16_t)((challenge[0] << 8) | challenge[1]), key, sizeof(key));
	hexdump("key", key, sizeof(key));

	if (vcmd(d, pm_group, PM_OP_KEY, pcs, 0, (uint16_t)sizeof(key), 0,
		 SG_DXFER_TO_DEV, key, sizeof(key)) < 0)
		return -1;
	printf("key accepted\n");
	return 0;
}

/* ------------------------------------------------------------------ */
/* usage                                                               */
/* ------------------------------------------------------------------ */

static void usage(void)
{
	printf(
"usage: %s <command> [options]\n"
"\n"
"commands:\n"
"  list                        list SCSI devices and show which are IS903\n"
"  info                        read the IS903 identification (INQU)\n"
"  capacity                    standard SCSI READ CAPACITY of the disc\n"
"  status                      query the P.M. status/info sub commands\n"
"  dump  <out.iso>                          extract the ISO image (VERIFIED)\n"
"  burn  <in.iso>                           burn an ISO image (VERIFIED, DESTRUCTIVE)\n"
"  read  <lba> <blocks> <out.iso>           raw P.M. read (vendor protocol)\n"
"  write <lba> <in.iso>                     raw P.M. write (vendor protocol)\n"
"  key                         perform the 12 byte key exchange\n"
"  probe                       try every command group / sub opcode\n"
"  raw   <hex-cdb> [dir] [len]   send an arbitrary vendor CDB\n"
"\n"
"options:\n"
"  --dev <node|index>   target device, default is the first IS903 found\n"
"  --pcs <n>            PCS slot for the key exchange (0-5)\n"
"  --force              required for destructive writes\n"
"\n"
"dir for raw is: r = from device, w = to device, n = none (default r)\n",
	      progname);
}

static struct dev_info *pick_dev(struct dev_info *list, int count,
				 const char *arg, bool need_is903)
{
	int i;
	struct dev_info *found = NULL;

	if (arg && arg[0] != '/') {
		int idx = atoi(arg);

		for (i = 0; i < count; i++)
			if (list[i].index == idx)
				found = &list[i];
	} else if (arg) {
		for (i = 0; i < count; i++)
			if (!strcmp(list[i].sg_path, arg))
				found = &list[i];
	}
	if (!found) {
		/* fall back to the first IS903 */
		for (i = 0; i < count; i++)
			if (dev_is_is903(&list[i])) {
				found = &list[i];
				break;
			}
	}
	if (!found)
		die("no matching device found (use '%s list')", progname);
	if (need_is903 && !dev_is_is903(found))
		fprintf(stderr, "%s: warning: %s does not look like an IS903\n",
			progname, found->sg_path);
	return found;
}

int main(int argc, char **argv)
{
	struct dev_info *list;
	int count = 0, i;
	const char *cmd;
	const char *devarg = NULL;
	const char *pos[8];
	int npos = 0;
	int pcs = 0;
	bool force = false;
	struct is903 dev;
	int rc = 0;

	if (argc < 2) {
		usage();
		return 1;
	}
	cmd = argv[1];
	for (i = 2; i < argc; i++) {
		if (!strcmp(argv[i], "--force")) {
			force = true;
		} else if (!strcmp(argv[i], "--dev") && i + 1 < argc) {
			devarg = argv[++i];
		} else if (!strcmp(argv[i], "--pcs") && i + 1 < argc) {
			pcs = atoi(argv[++i]);
		} else if (npos < (int)(sizeof(pos) / sizeof(pos[0]))) {
			pos[npos++] = argv[i];
		} else {
			die("too many arguments");
		}
	}

	if (!strcmp(cmd, "-h") || !strcmp(cmd, "--help")) {
		usage();
		return 0;
	}

	list = discover(&count);
	if (count == 0)
		fprintf(stderr, "%s: warning: no SCSI generic devices found.\n"
			"       Load the sg module first: sudo modprobe sg\n",
			progname);

	if (!strcmp(cmd, "list")) {
		printf("%-10s %-12s %-10s %-4s %s\n", "NODE", "VENDOR", "PRODUCT",
		       "TYPE", "NOTE");
		for (i = 0; i < count; i++) {
			printf("%-10s %-12s %-10s %-4d %s\n",
			       list[i].sg_path, list[i].vendor, list[i].product,
			       list[i].type,
			       dev_is_is903(&list[i]) ? "<- IS903" : "");
			if (list[i].sr_path[0])
				printf("           %s\n", list[i].sr_path);
		}
		free(list);
		return 0;
	}

	/* everything else needs a device */
	{
		struct dev_info *di = pick_dev(list, count, devarg, true);

		/* burn and dump talk to the block device directly, not to
		 * /dev/sgN, so they work even without access to the sg node. */
		if (!strcmp(cmd, "dump")) {
			if (npos < 1)
				die("dump needs <out.iso>");
			if (di->type == 5) {
				fprintf(stderr, "%s: %s is a CD-ROM (read only); "
					"dumping needs the mass-storage mode\n",
					progname, di->sg_path);
				rc = -1;
			} else if (!di->sr_path[0]) {
				fprintf(stderr, "%s: no block device found for %s\n",
					progname, di->sg_path);
				rc = -1;
			} else {
				rc = blk_read_iso(di->sr_path, pos[0]);
			}
			free(list);
			return rc < 0 ? 1 : rc;
		}
		if (!strcmp(cmd, "burn")) {
			if (npos < 1)
				die("burn needs <in.iso>");
			if (!force)
				die("refusing to write to the device without --force");
			if (di->type == 5) {
				fprintf(stderr, "%s: %s is a CD-ROM (read only); "
					"burning needs the mass-storage mode\n",
					progname, di->sg_path);
				rc = -1;
			} else if (!di->sr_path[0]) {
				fprintf(stderr, "%s: no block device found for %s\n",
					progname, di->sg_path);
				rc = -1;
			} else {
				rc = blk_write_iso(di->sr_path, pos[0]);
			}
			free(list);
			return rc < 0 ? 1 : rc;
		}

		is903_open(&dev, di->sg_path);
		printf("%s: using %s (%s %s)\n", progname, di->sg_path,
		       di->vendor, di->product);

		if (!strcmp(cmd, "info")) {
			rc = cmd_inqu(&dev);
		} else if (!strcmp(cmd, "capacity")) {
			rc = cmd_capacity(&dev);
		} else if (!strcmp(cmd, "status")) {
			if (discover_groups(&dev, false) < 0)
				die("could not determine the command groups "
				    "supported by the device");
			rc = cmd_status(&dev);
		} else if (!strcmp(cmd, "key")) {
			if (discover_groups(&dev, false) < 0)
				die("could not determine the command groups "
				    "supported by the device");
			rc = cmd_key(&dev, pcs);
		} else if (!strcmp(cmd, "read")) {
			if (npos < 3)
				die("read needs <lba> <blocks> <out.iso>");
			if (discover_groups(&dev, false) < 0)
				die("could not determine the command groups "
				    "supported by the device");
			rc = cmd_read(&dev, (uint32_t)strtoul(pos[0], NULL, 0),
				      (uint32_t)strtoul(pos[1], NULL, 0), pos[2]);
		} else if (!strcmp(cmd, "write")) {
			if (npos < 2)
				die("write needs <lba> <in.iso>");
			if (!force)
				die("refusing to write to the device without --force");
			if (discover_groups(&dev, false) < 0)
				die("could not determine the command groups "
				    "supported by the device");
			rc = cmd_write(&dev, (uint32_t)strtoul(pos[0], NULL, 0),
				       pos[1]);
		} else if (!strcmp(cmd, "raw")) {
			int dir = SG_DXFER_FROM_DEV;
			uint32_t len = 512;

			if (!pos[0])
				die("raw needs a hex CDB");
			if (pos[1]) {
				switch (pos[1][0]) {
				case 'r': dir = SG_DXFER_FROM_DEV; break;
				case 'w': dir = SG_DXFER_TO_DEV; break;
				case 'n': dir = SG_DXFER_NONE; break;
				default: die("dir must be r, w or n");
				}
			}
			if (pos[2])
				len = (uint32_t)strtoul(pos[2], NULL, 0);
			rc = cmd_raw(&dev, pos[0], dir, len);
		} else if (!strcmp(cmd, "probe")) {
			int g, ngroups;

			/* Brute forcing vendor commands can wedge the device
			 * firmware, so this has to be requested explicitly. */
			if (!force)
				die("probe sends many vendor commands and can hang "
				    "the device firmware; re-run with --force if you "
				    "are sure");
			if (discover_groups(&dev, true) < 0)
				die("could not determine the command groups "
				    "supported by the device");
			ngroups = GRP_MAX;
			for (g = 0; g < ngroups; g++) {
				int op;

				for (op = 0; op < 16; op++) {
					unsigned char buf[512];

					memset(buf, 0, sizeof(buf));
					if (vcmd(&dev, g, op, 0, 0, 0x0080, 0,
						 SG_DXFER_FROM_DEV, buf,
						 sizeof(buf)) < 0)
						continue;
					printf("group %d (%s) op 0x%02x:\n", g,
					       cur_tag[g], op);
					hexdump("  reply", buf, 64);
				}
			}
		} else {
			usage();
			rc = 1;
		}
		is903_close(&dev);
	}
	free(list);
	return rc < 0 ? 1 : rc;
}
