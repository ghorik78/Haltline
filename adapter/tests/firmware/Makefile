# Builds the end-to-end test firmware (the tests also build it on their own).
ARM_CC ?= arm-none-eabi-gcc
CFLAGS = -mcpu=cortex-m4 -mthumb -O0 -g3 -ffreestanding -fno-common -Wall

firmware.elf: main.c startup.c linker.ld
	$(ARM_CC) $(CFLAGS) -nostdlib -T linker.ld startup.c main.c -lgcc -o $@

clean:
	rm -f firmware.elf

.PHONY: clean
