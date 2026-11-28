#!/bin/sh
# Copyright (C) 2026 dekajoule
# SPDX-License-Identifier: GPL-2.0-or-later
# sdcard-clone.sh - clone the scope's boot SD card on macOS or Linux, and write a clone to the second card.
# DISK is the whole card: diskN on macOS, sdX or mmcblkN on Linux (with or without /dev/).
#
#   sdcard-clone.sh list                       list removable SD and USB disks
#   sdcard-clone.sh read  DISK  out.img        card -> image (+ out.img.sha256, + layout report)
#   sdcard-clone.sh write in.img DISK          image -> card (asks you to type the disk id), then verifies
#   sdcard-clone.sh verify in.img DISK         compare a card with an image (sha256 over the image length)
#   sdcard-clone.sh fit in.img DISK|BYTES out.img
#                                              a copy of a card image for a smaller card: the ext4 of the last
#                                              partition (userdata) shrunk to what is left, in Docker. in.img is only read
#
# Safety: refuses the disk of the running system and any disk that is not removable/external SD or USB media (on
# Linux: USB with removable or hotplug set, or an mmcblk of type SD, not an eMMC); refuses to write an image that is
# larger than the card (fit makes one that is not); never runs without an explicit DISK.
set -eu

die() { echo "error: $*" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib/docker.sh"

os=$(uname -s)
case "$os" in Darwin|Linux) ;; *) die "macOS or Linux only, not $os";; esac

dev_name() { echo "${1#/dev/}"; } # disk4, /dev/disk4, sdX and /dev/sdX all name the disk

# shasum where Perl brings it (macOS, most Linux), else sha256sum of coreutils (#67)
sha256() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@"; else sha256sum "$@"; fi | cut -d' ' -f1; }
sha256_check() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 -c "$1"; else sha256sum -c "$1"; fi; }

if [ "$os" = Darwin ]; then
	info() { diskutil info "/dev/$1" 2>/dev/null; }
	field() { info "$1" | awk -F': *' -v k="$2" '$1 ~ "^ *"k"$" {print $2; exit}'; }

	check_disk() {
		d=$1
		case "$d" in disk[0-9]*) ;; *) die "give a whole disk like disk4, not '$d'";; esac
		case "$d" in *s[0-9]*) die "'$d' is a partition, give the whole disk";; esac
		info "$d" >/dev/null || die "no such disk: $d"
		boot=$(diskutil info / | awk -F': *' '/Part of Whole/ {print $2}')
		[ "$d" != "$boot" ] || die "$d holds the running macOS"
		proto=$(field "$d" "Protocol")
		removable=$(field "$d" "Removable Media")
		location=$(field "$d" "Device Location")
		virtual=$(field "$d" "Virtual")
		[ "$virtual" != "Yes" ] || die "$d is a virtual disk (disk image)"
		case "$proto" in
			"Secure Digital"|USB) ;;
			*) die "$d protocol is '$proto' (expected Secure Digital or USB)";;
		esac
		case "$removable$location" in
			*Removable*|*External*) ;;
			*) die "$d is neither removable nor external ($removable / $location)";;
		esac
	}

	disk_bytes() { field "$1" "Disk Size" | sed -E 's/.*\(([0-9]+) Bytes\).*/\1/'; }

	describe() {
		d=$1
		printf "%s: %s, %s bytes, protocol %s, %s\n" "$d" "$(field "$d" "Device / Media Name")" "$(disk_bytes "$d")" \
			"$(field "$d" "Protocol")" "$(field "$d" "Removable Media")"
	}

	list_disks() {
		diskutil list physical | awk '/^\/dev\/disk/ {print $1}' | sed 's|/dev/||' | while read -r d; do
			p=$(field "$d" "Protocol"); case "$p" in "Secure Digital"|USB) echo "$d";; esac
		done
	}

	raw_dev() { echo "/dev/r$1"; }
	unmount_disk() { diskutil unmountDisk "/dev/$1" >/dev/null; }
	eject_disk() { diskutil eject "/dev/$1" >/dev/null && echo "ejected $1"; }
	file_bytes() { stat -f %z "$1"; }
else
	lsb() { lsblk -dno "$2" "/dev/$1" 2>/dev/null | sed 's/^ *//; s/ *$//'; }

	check_disk() {
		d=$1
		[ -n "$d" ] && [ -b "/dev/$d" ] || die "no such disk: $d"
		type=$(lsb "$d" TYPE)
		[ "$type" != part ] || die "'$d' is a partition, give the whole disk"
		[ "$type" = disk ] || die "$d is not a whole disk (lsblk type '$type')"
		[ -e "/sys/block/$d/device" ] || die "$d is a virtual disk (no device behind it)"
		# the running system: /, /boot, /usr, /var, /home, ... or swap on the disk or on anything built on it
		sys=$(lsblk -lno MOUNTPOINT "/dev/$d" 2>/dev/null |
			awk '$0 ~ /^(\/|\/(boot|efi|usr|var|home|opt|srv|etc|root)(\/.*)?|\[SWAP\])$/ {print; exit}')
		[ -z "$sys" ] || die "$d holds the running system ($sys)"
		# in use without a mountpoint of its own: held by device-mapper or md (LVM, crypt, RAID), or a member of a ZFS
		# pool, an LVM volume group, a RAID or a btrfs of several devices
		for h in /sys/block/"$d"/holders/* /sys/block/"$d"/"$d"*/holders/*; do
			if [ -e "$h" ]; then die "$d is in use (held by ${h##*/})"; fi
		done
		m=$(lsblk -lno FSTYPE "/dev/$d" 2>/dev/null | grep -Ex 'zfs_member|LVM2_member|linux_raid_member' | head -n 1 || true)
		[ -z "$m" ] || die "$d is in use or part of a pool ($m)"
		for b in /sys/fs/btrfs/*/devices/*; do
			case "${b##*/}" in "$d"|"$d"[0-9]*|"$d"p[0-9]*) die "$d is in use (a device of a btrfs)";; esac
		done
		case "$d" in
			mmcblk*)
				t=$(cat "/sys/block/$d/device/type" 2>/dev/null || true)
				[ "$t" = SD ] || die "$d is not an SD card (type '$t'; an eMMC is not removable)";;
			*)
				tran=$(lsb "$d" TRAN); rm=$(lsb "$d" RM); hp=$(lsb "$d" HOTPLUG)
				[ "$tran" = usb ] || die "$d is a '$tran' disk, not removable SD or USB media"
				[ "$rm" = 1 ] || [ "$hp" = 1 ] || die "$d is not removable (removable $rm, hotplug $hp)";;
		esac
		[ "$(disk_bytes "$d")" -gt 0 ] || die "$d has no medium (an empty card slot?)"
	}

	disk_bytes() { lsblk -bdno SIZE "/dev/$1" 2>/dev/null | tr -d ' '; }

	describe() {
		d=$1
		printf "%s: %s, %s bytes, %s, removable %s\n" "$d" "$(lsb "$d" MODEL)" "$(disk_bytes "$d")" \
			"$(lsb "$d" TRAN)" "$(lsb "$d" RM)"
	}

	list_disks() {
		for p in /sys/block/*; do
			d=${p##*/}
			(check_disk "$d") >/dev/null 2>&1 && echo "$d"
		done
		true
	}

	raw_dev() { echo "/dev/$1"; }

	# as diskutil unmountDisk: every mounted file system on the card (udisks for a desktop's automount, else umount),
	# and it fails when one stays mounted (lsblk shows one mount of each partition, a partition can have more)
	unmount_disk() {
		lsblk -lnpo NAME,MOUNTPOINT "/dev/$1" | awk 'NF >= 2 {print $1}' | while read -r p; do
			udisksctl unmount -b "$p" </dev/null >/dev/null 2>&1 || sudo umount "$p" </dev/null || die "cannot unmount $p"
		done
		left=$(lsblk -lno MOUNTPOINT "/dev/$1" | awk 'NF {print; exit}')
		[ -z "$left" ] || die "$1 is still mounted at $left: unmount it and try again"
	}

	eject_disk() { sync; echo "$1 can be taken out"; }
	file_bytes() { stat -c %s "$1"; }
fi

copy() { # copy src dst bytes
	if command -v pv >/dev/null 2>&1; then
		pv -s "$3" "$1" | dd of="$2" bs=4194304 2>/dev/null
	else
		dd if="$1" of="$2" bs=4194304 status=progress
	fi
}

cmd=${1:-}
case "$cmd" in
list)
	list_disks | while read -r d; do describe "$d"; done
	;;
read)
	[ $# -eq 3 ] || die "usage: $0 read DISK out.img"
	d=$(dev_name "$2"); out=$3
	check_disk "$d"; describe "$d"
	[ ! -e "$out" ] || die "$out exists, refusing to overwrite"
	bytes=$(disk_bytes "$d")
	free=$(df -k "$(dirname "$out")" | awk 'NR==2 {print $4 * 1024}')
	[ "$free" -gt "$bytes" ] || die "not enough free space for $bytes bytes next to $out"
	unmount_disk "$d"
	raw=$(raw_dev "$d")
	echo "reading $raw -> $out ($bytes bytes)"
	if command -v pv >/dev/null 2>&1; then
		sudo sh -c 'pv -s "$1" "$2" > "$3"' sh "$bytes" "$raw" "$out"
	else
		sudo dd if="$raw" of="$out" bs=4194304 status=progress
	fi
	sudo chown "$(id -u):$(id -g)" "$out"
	[ "$(file_bytes "$out")" -eq "$bytes" ] || die "image size differs from the card size"
	echo "hashing image and card ..."
	h_img=$(sha256 "$out")
	h_dev=$(sudo dd if="$raw" bs=4194304 2>/dev/null | sha256)
	[ "$h_img" = "$h_dev" ] || die "sha256 mismatch: image $h_img, card $h_dev"
	echo "$h_img  $(basename "$out")" > "$out.sha256"
	echo "ok: sha256 $h_img"
	py="$here/.venv/bin/python"; [ -x "$py" ] || py=python3
	"$py" "$here/sdcard-layout.py" "$out" | tee "$out.layout.txt"
	;;
write)
	[ $# -eq 3 ] || die "usage: $0 write in.img DISK"
	img=$2; d=$(dev_name "$3")
	[ -f "$img" ] || die "no image $img"
	check_disk "$d"; describe "$d"
	ib=$(file_bytes "$img"); db=$(disk_bytes "$d")
	[ "$ib" -le "$db" ] || die "image ($ib bytes) is larger than the card ($db bytes); for a smaller card: $0 fit $img $d fitted.img"
	if [ -f "$img.sha256" ] && [ "${IMAGE_VERIFIED:-0}" != 1 ]; then
		echo "checking $img against $img.sha256 ..."
		(cd "$(dirname "$img")" && sha256_check "$(basename "$img").sha256") || die "image does not match its .sha256"
	fi
	echo "ALL DATA ON $d WILL BE REPLACED by $img."
	printf "type the disk id (%s) to continue: " "$d"; read -r answer
	[ "$answer" = "$d" ] || die "aborted"
	unmount_disk "$d"
	raw=$(raw_dev "$d")
	if command -v pv >/dev/null 2>&1; then
		sudo sh -c 'pv -s "$1" "$2" > "$3"' sh "$ib" "$img" "$raw"
	else
		sudo dd if="$img" of="$raw" bs=4194304 status=progress
	fi
	sync
	"$0" verify "$img" "$d"
	eject_disk "$d"
	;;
verify)
	[ $# -eq 3 ] || die "usage: $0 verify in.img DISK"
	img=$2; d=$(dev_name "$3")
	check_disk "$d"
	[ -f "$img" ] || die "no image $img"
	ib=$(file_bytes "$img")
	unmount_disk "$d" 2>/dev/null || true
	if [ -f "$img.sha256" ] && [ "${IMAGE_VERIFIED:-0}" = 1 ]; then h_img=$(cut -d' ' -f1 "$img.sha256"); else h_img=$(sha256 "$img"); fi
	h_dev=$(sudo dd if="$(raw_dev "$d")" bs=4194304 count=$(( (ib + 4194303) / 4194304 )) 2>/dev/null | head -c "$ib" | sha256)
	[ "$h_img" = "$h_dev" ] && echo "verify ok: $h_img" || die "verify FAILED: image $h_img, card $h_dev"
	;;
fit)
	[ $# -eq 4 ] || die "usage: $0 fit in.img DISK|BYTES out.img"
	img=$2; target=$3; out=$4
	[ -f "$img" ] || die "no image $img"
	[ ! -e "$out" ] || die "$out exists, refusing to overwrite"
	case "$target" in
		""|*[!0-9]*) td=$(dev_name "$target"); check_disk "$td"; tb=$(disk_bytes "$td");;
		*) tb=$target;;
	esac
	ib=$(file_bytes "$img")
	[ "$ib" -gt "$tb" ] || die "the image fits as it is ($ib <= $tb bytes): $0 write $img DISK"
	# the last partition of the stock table (PARM at LBA 8192, offsets relative to it): it has to run to the end of the
	# card ("-"), so a smaller card only makes it shorter, and it has to be the only one past the new end
	last=$(python3 -I - "$img" <<'EOF'
import re, struct, sys
f = open(sys.argv[1], "rb"); f.seek(8192 * 512); raw = f.read(64 * 512)
if raw[:4] != b"PARM":
    sys.exit("no PARM block at LBA 8192: not a DHO card image")
txt = raw[8:8 + struct.unpack_from("<I", raw, 4)[0]].decode("latin1")
m = re.search(r"mtdparts=[^:]+:(\S+)", txt)
parts = [re.match(r"(-|0x[0-9a-fA-F]+)@(0x[0-9a-fA-F]+)\(([^):]+)", p) for p in m.group(1).split(",")] if m else []
if not parts or not all(parts):
    sys.exit("no readable partition table in PARM")
size, off, name = parts[-1].groups()
if size != "-":
    sys.exit(f"the last partition ({name}) has a fixed size, it does not end with the card")
print(name, 8192 + int(off, 16))
EOF
) || die "cannot read the partition table of $img"
	name=${last% *}; lba=${last#* }
	off=$((lba * 512))
	# whole 4 MiB below the end of the new card
	avail=$(( (tb - off) / 4194304 * 4194304 ))
	[ "$avail" -gt 0 ] || die "$name starts at $off bytes, past the end of $tb bytes"
	free=$(df -k "$(dirname "$out")" | awk 'NR==2 {print $4 * 1024}')
	[ "$free" -gt "$tb" ] || die "not enough free space for $tb bytes next to $out"
	docker info >/dev/null 2>&1 || die "Docker is not running"
	echo "$name at LBA $lba: its ext4 shrunk to $avail bytes, for a card of $tb bytes"
	indir=$(cd "$(dirname "$img")" && pwd); outdir=$(mkdir -p "$(dirname "$out")" && cd "$(dirname "$out")" && pwd)
	# the partition is copied into a Docker volume (sparse: only what is used takes room), checked and shrunk there;
	# in.img is mounted read only
	docker run --rm -e IMG="$(basename "$img")" -e OUT="$(basename "$out")" -e OFF="$off" -e AVAIL="$avail" -e NAME="$name" \
		-e OUT_OWNER="$(id -u):$(id -g)" -v "$indir:/in:ro" -v "$outdir:/out" -v oos-sdfit:/w "$OOS_ALPINE" sh -euc '
		apk add --no-cache e2fsprogs e2fsprogs-extra coreutils >/dev/null
		p=/w/part; rm -f $p
		trap "rm -f $p" EXIT
		dd if="/in/$IMG" of=$p bs=4M iflag=skip_bytes skip="$OFF" conv=sparse status=none
		fsbytes() { dumpe2fs -h $p 2>/dev/null | awk -F: "/^Block count:/ {c=\$2} /^Block size:/ {s=\$2} END {print c * s}"; }
		[ "$(fsbytes)" -gt 0 ] || { echo "$NAME holds no ext2/3/4 file system"; exit 1; }
		# resize2fs wants a freshly checked file system; this changes only the copy
		rc=0; e2fsck -f -y $p >/w/fsck.log 2>&1 || rc=$?
		[ $rc -le 1 ] || { tail -20 /w/fsck.log; echo "e2fsck returned $rc on $NAME"; exit 1; }
		# 1 is usual: the stock system is switched off at the mains, its journal is replayed
		[ $rc = 0 ] || echo "e2fsck repaired the copy of $NAME, usually the journal of the last power-off (/w/fsck.log in the volume oos-sdfit)"
		if [ "$(fsbytes)" -gt "$AVAIL" ]; then
			resize2fs $p "$((AVAIL / 1024))K" >/w/resize.log 2>&1 || { cat /w/resize.log; echo "the files on $NAME need more than $AVAIL bytes"; exit 1; }
		fi
		e2fsck -f -n $p >/dev/null 2>&1 || { echo "$NAME is not clean after the resize"; exit 1; }
		n=$(fsbytes)
		dd if="/in/$IMG" of="/out/$OUT" bs=4M iflag=count_bytes count="$OFF" status=none
		dd if=$p of="/out/$OUT" bs=4M iflag=count_bytes count="$n" oflag=seek_bytes seek="$OFF" conv=sparse,notrunc status=none
		truncate -s $((OFF + n)) "/out/$OUT"
		chown "$OUT_OWNER" "/out/$OUT"
		echo "$NAME: $n bytes of file system, image $((OFF + n)) bytes"
	' || { rm -f "$out"; die "fit failed, $img is unchanged"; }
	ob=$(file_bytes "$out")
	[ "$ob" -le "$tb" ] || die "$out came out larger than the card ($ob > $tb)"
	h=$(sha256 "$out")
	echo "$h  $(basename "$out")" > "$out.sha256"
	echo "ok: $out ($ob bytes, sha256 $h). Next: $0 write $out DISK"
	;;
*)
	sed -n '2,17p' "$0"; exit 1;;
esac
