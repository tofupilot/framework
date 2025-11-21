import random
import statistics
import re
import time


def test_ain1(measurements):
    print("Reading voltage from Ain1...")

    # Simulate 10 voltage readings
    voltages = [random.uniform(3.2, 3.4) for _ in range(10)]

    measurements.mean_voltage = statistics.mean(voltages)
    measurements.std_dev = statistics.stdev(voltages)
    measurements.min_voltage = min(voltages)
    measurements.max_voltage = max(voltages)
    measurements.resolution = abs(voltages[0] - voltages[1])

    print(f"Mean voltage: {measurements.mean_voltage:.3f}V")
    print(f"Stability (std dev): {measurements.std_dev:.4f}V")
    time.sleep(1)


def check_mac_address(measurements):
    print("Reading Ethernet MAC address...")

    # Simulate reading MAC (in reality: read from device)
    measurements.mac_address = "AC:DE:48:12:34:56"
    measurements.oui = measurements.mac_address[:8]
    # Check if unicast (not multicast)
    first_byte = int(measurements.mac_address.split(":")[0], 16)
    measurements.unicast = first_byte & 0x01
    time.sleep(2)


def check_wifi_mac(measurements):
    print("Reading WiFi MAC address...")

    # Simulate reading WiFi MAC (usually consecutive to Ethernet MAC)
    measurements.wifi_mac = "AC:DE:48:12:34:57"
    print(f"WiFi MAC: {measurements.wifi_mac}")
    time.sleep(2.5)


def check_imei(measurements):
    """Check IMEI number"""
    print("Reading IMEI...")

    # Simulate reading IMEI (15 digits)
    measurements.imei = "354987654321098"

    # Check length
    measurements.imei_length = len(measurements.imei)
    measurements.imei_is_numeric = measurements.imei.isdigit()

    # Luhn checksum validation
    def luhn_check(number):
        digits = [int(d) for d in number]
        checksum = 0
        for i, digit in enumerate(reversed(digits)):
            if i % 2 == 1:
                digit *= 2
                if digit > 9:
                    digit -= 9
            checksum += digit
        return checksum % 10 == 0

    measurements.imei_checksum_valid = luhn_check(measurements.imei)

    # Extract TAC (Type Allocation Code - first 8 digits)
    measurements.tac = measurements.imei[:8]

    print(f"IMEI: {measurements.imei}")
    print(f"TAC: {measurements.tac}")
    print(f"Checksum valid: {measurements.imei_checksum_valid}")
    time.sleep(3)
