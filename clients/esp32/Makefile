ESP32_TOOL := ../../tools/esp32

.PHONY: setup doctor menuconfig build flash monitor flash-monitor clean

setup:
	$(ESP32_TOOL) setup

doctor:
	$(ESP32_TOOL) doctor

menuconfig:
	$(ESP32_TOOL) menuconfig

build:
	$(ESP32_TOOL) build

flash:
	$(ESP32_TOOL) flash $(PORT)

monitor:
	$(ESP32_TOOL) monitor $(PORT)

flash-monitor:
	$(ESP32_TOOL) flash-monitor $(PORT)

clean:
	$(ESP32_TOOL) clean
