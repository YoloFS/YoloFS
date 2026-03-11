KDIR ?= /lib/modules/6.8.0-101-generic/build

obj-m += agfs.o
agfs-y := super.o inode.o file.o dentry.o lookup.o staging.o perm.o ioctl.o log.o

all:
	$(MAKE) -C $(KDIR) M=$(CURDIR) KBUILD_MODPOST_WARN=1 modules

clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) clean
