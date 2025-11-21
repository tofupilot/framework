"""
IXrouter3 Hardware Test using OpenHTF
Electrical and Functional Testing
"""

import openhtf as htf
from openhtf import plugs
from openhtf.util import units
import random
import statistics
import re
from tofupilot.openhtf import TofuPilot


# ==================== PLUGS ====================


class PowerSupply(plugs.BasePlug):
    """Power supply instrument plug"""

    def read_voltage(self, rail):
        """Read voltage from power rail"""
        # In reality: communicate with actual power supply
        voltages = {
            "3.3V": random.uniform(3.25, 3.35),
            "5V": random.uniform(4.95, 5.05),
            "12V": random.uniform(11.9, 12.1),
        }
        return voltages.get(rail, 0.0)

    def read_current(self, rail):
        """Read current from power rail"""
        # Simulate current readings
        return random.uniform(50, 200)


class DMM(plugs.BasePlug):
    """Digital Multimeter plug"""

    def measure_voltage(self, channel):
        """Measure voltage on a channel"""
        return random.uniform(4.95, 5.05)

    def measure_resistance(self, points):
        """Measure resistance between two points"""
        return random.uniform(4465, 4935)

    def measure_isolation(self, rail1, rail2):
        """Measure isolation resistance"""
        return random.uniform(5, 20)  # MΩ


class DUT(plugs.BasePlug):
    """Device Under Test plug"""

    def read_mac_address(self):
        """Read Ethernet MAC address"""
        return "AC:DE:48:12:34:56"

    def read_wifi_mac(self):
        """Read WiFi MAC address"""
        return "AC:DE:48:12:34:57"

    def read_imei(self):
        """Read IMEI number"""
        return "354987654321098"

    def read_digital_input(self, pin):
        """Read digital input pin"""
        return (
            random.uniform(3.1, 3.3)
            if random.choice([True, False])
            else random.uniform(0.0, 0.2)
        )

    def set_digital_output(self, pin, state):
        """Set digital output pin"""
        pass

    def measure_digital_output(self, pin):
        """Measure digital output voltage"""
        return random.uniform(3.2, 3.3) if pin else random.uniform(0.0, 0.1)


# ==================== TEST PHASES ====================


@htf.measures(
    htf.Measurement("voltage_3v3_mean")
    .in_range(3.25, 3.35)
    .with_units(units.Unit("V")),
    htf.Measurement("voltage_3v3_ripple").in_range(0, 0.05).with_units(units.Unit("V")),
    htf.Measurement("voltage_5v_mean").in_range(4.95, 5.05).with_units(units.Unit("V")),
    htf.Measurement("voltage_5v_ripple").in_range(0, 0.05).with_units(units.Unit("V")),
    htf.Measurement("voltage_12v_mean")
    .in_range(11.9, 12.1)
    .with_units(units.Unit("V")),
    htf.Measurement("voltage_12v_ripple").in_range(0, 0.1).with_units(units.Unit("V")),
)
@htf.plug(psu=PowerSupply)
def test_power_rails(test, psu):
    """Test all power supply voltages"""
    print("Testing power rails...")

    # Test 3.3V rail
    voltage_3v3 = [psu.read_voltage("3.3V") for _ in range(10)]
    measurements.voltage_3v3_mean = statistics.mean(voltage_3v3)
    measurements.voltage_3v3_ripple = max(voltage_3v3) - min(voltage_3v3)

    # Test 5V rail
    voltage_5v = [psu.read_voltage("5V") for _ in range(10)]
    measurements.voltage_5v_mean = statistics.mean(voltage_5v)
    measurements.voltage_5v_ripple = max(voltage_5v) - min(voltage_5v)

    # Test 12V rail
    voltage_12v = [psu.read_voltage("12V") for _ in range(10)]
    measurements.voltage_12v_mean = statistics.mean(voltage_12v)
    measurements.voltage_12v_ripple = max(voltage_12v) - min(voltage_12v)

    print(f"3.3V: {measurements.voltage_3v3_mean:.3f}V")
    print(f"5V: {measurements.voltage_5v_mean:.3f}V")
    print(f"12V: {measurements.voltage_12v_mean:.3f}V")


@htf.measures(
    htf.Measurement("current_3v3_idle").in_range(40, 60).with_units(units.Unit("mA")),
    htf.Measurement("current_3v3_active")
    .in_range(150, 250)
    .with_units(units.Unit("mA")),
    htf.Measurement("current_5v_idle").in_range(0, 35).with_units(units.Unit("mA")),
    htf.Measurement("current_5v_active").in_range(70, 130).with_units(units.Unit("mA")),
    htf.Measurement("current_12v_idle").in_range(0, 15).with_units(units.Unit("mA")),
    htf.Measurement("current_12v_active")
    .in_range(140, 210)
    .with_units(units.Unit("mA")),
    htf.Measurement("power_idle").in_range(0, 500).with_units(units.Unit("mW")),
    htf.Measurement("power_active").in_range(0, 3000).with_units(units.Unit("mW")),
)
@htf.plug(psu=PowerSupply)
def test_current_consumption(test, psu):
    """Test current draw on all rails"""
    print("Measuring current consumption...")

    # Idle current
    measurements.current_3v3_idle = random.uniform(45, 55)
    measurements.current_5v_idle = random.uniform(20, 30)
    measurements.current_12v_idle = random.uniform(5, 10)

    # Active current
    measurements.current_3v3_active = random.uniform(180, 220)
    measurements.current_5v_active = random.uniform(80, 120)
    measurements.current_12v_active = random.uniform(150, 200)

    # Calculate power
    measurements.power_idle = (
        measurements.current_3v3_idle * 3.3
        + measurements.current_5v_idle * 5.0
        + measurements.current_12v_idle * 12.0
    )

    measurements.power_active = (
        measurements.current_3v3_active * 3.3
        + measurements.current_5v_active * 5.0
        + measurements.current_12v_active * 12.0
    )

    print(f"Idle power: {measurements.power_idle:.1f}mW")
    print(f"Active power: {measurements.power_active:.1f}mW")


@htf.measures(
    htf.Measurement("mean_voltage").in_range(3.2, 3.4).with_units(units.Unit("V")),
    htf.Measurement("std_dev").in_range(0, 0.2),
    htf.Measurement("min_voltage"),
    htf.Measurement("max_voltage"),
    htf.Measurement("resolution").in_range(0, 0.5).with_units(units.Unit("V")),
)
@htf.plug(dmm=DMM)
def test_ain1(test, dmm):
    """Test analog input channels accuracy"""
    print("Testing analog inputs...")

    # Simulate 10 voltage readings
    voltages = [random.uniform(3.2, 3.4) for _ in range(10)]

    measurements.mean_voltage = statistics.mean(voltages)
    measurements.std_dev = statistics.stdev(voltages)
    measurements.min_voltage = min(voltages)
    measurements.max_voltage = max(voltages)
    measurements.resolution = abs(voltages[0] - voltages[1])


@htf.measures(
    htf.Measurement("aout0_measured").in_range(2.45, 2.55).with_units(units.Unit("V")),
    htf.Measurement("aout0_error").in_range(0, 0.05).with_units(units.Unit("V")),
    htf.Measurement("aout0_impedance").in_range(40, 60).with_units(units.OHM),
    htf.Measurement("aout1_measured").in_range(2.45, 2.55).with_units(units.Unit("V")),
    htf.Measurement("aout1_error").in_range(0, 0.05).with_units(units.Unit("V")),
    htf.Measurement("aout1_impedance").in_range(40, 60).with_units(units.OHM),
)
@htf.plug(dmm=DMM)
def test_analog_outputs(test, dmm):
    """Test DAC outputs"""
    print("Testing analog outputs...")

    target_voltage = 2.5

    for channel in range(2):
        measured = random.uniform(2.48, 2.52)
        impedance = random.uniform(45, 55)

        setattr(test.measurements, f"aout{channel}_measured", measured)
        setattr(
            test.measurements, f"aout{channel}_error", abs(measured - target_voltage)
        )
        setattr(test.measurements, f"aout{channel}_impedance", impedance)

        print(f"AOUT{channel}: {measured:.3f}V, Z={impedance:.1f}Ω")


@htf.measures(
    htf.Measurement("digital_inputs_tested").equals(8),
    htf.Measurement("digital_inputs_passed").equals(8),
    htf.Measurement("din0_high").in_range(2.0, 5.0).with_units(units.Unit("V")),
    htf.Measurement("din0_low").in_range(0, 0.8).with_units(units.Unit("V")),
)
@htf.plug(dut=DUT)
def test_digital_inputs(test, dut):
    """Test digital input pins"""
    print("Testing digital inputs...")

    measurements.digital_inputs_tested = 8
    passed = 0

    for pin in range(8):
        high_voltage = random.uniform(3.1, 3.3)
        low_voltage = random.uniform(0.0, 0.2)

        if pin == 0:  # Store first pin for validators
            measurements.din0_high = high_voltage
            measurements.din0_low = low_voltage

        if high_voltage > 2.0 and low_voltage < 0.8:
            passed += 1

    measurements.digital_inputs_passed = passed
    print(f"Digital inputs: {passed}/8 passed")


@htf.measures(
    htf.Measurement("digital_outputs_tested").equals(8),
    htf.Measurement("digital_outputs_passed").equals(8),
    htf.Measurement("dout0_high").in_range(2.4, 5.0).with_units(units.Unit("V")),
    htf.Measurement("dout0_low").in_range(0, 0.4).with_units(units.Unit("V")),
    htf.Measurement("dout0_source_current")
    .in_range(15, 100)
    .with_units(units.Unit("mA")),
)
@htf.plug(dut=DUT)
def test_digital_outputs(test, dut):
    """Test digital output pins"""
    print("Testing digital outputs...")

    measurements.digital_outputs_tested = 8
    passed = 0

    for pin in range(8):
        high_voltage = random.uniform(3.2, 3.3)
        low_voltage = random.uniform(0.0, 0.1)
        source_current = random.uniform(18, 22)

        if pin == 0:  # Store first pin for validators
            measurements.dout0_high = high_voltage
            measurements.dout0_low = low_voltage
            measurements.dout0_source_current = source_current

        if high_voltage > 2.4 and low_voltage < 0.4 and source_current > 15:
            passed += 1

    measurements.digital_outputs_passed = passed
    print(f"Digital outputs: {passed}/8 passed")


@htf.measures(
    htf.Measurement("pullup_scl").in_range(4465, 4935).with_units(units.OHM),
    htf.Measurement("pullup_sda").in_range(4465, 4935).with_units(units.OHM),
    htf.Measurement("pullup_reset").in_range(4465, 4935).with_units(units.OHM),
    htf.Measurement("pullup_boot").in_range(4465, 4935).with_units(units.OHM),
)
@htf.plug(dmm=DMM)
def test_pull_up_resistors(test, dmm):
    """Verify pull-up resistor values"""
    print("Testing pull-up resistors...")

    test_points = ["scl", "sda", "reset", "boot"]
    expected = 4700  # 4.7kΩ

    for point in test_points:
        measured = dmm.measure_resistance(point)
        setattr(test.measurements, f"pullup_{point}", measured)
        print(f"Pull-up {point.upper()}: {measured:.0f}Ω")


@htf.measures(
    htf.Measurement("isolation_3v3_to_gnd").in_range(1.0, 100).with_units(units.OHM),
    htf.Measurement("isolation_5v_to_gnd").in_range(1.0, 100).with_units(units.OHM),
    htf.Measurement("isolation_12v_to_gnd").in_range(1.0, 100).with_units(units.OHM),
    htf.Measurement("isolation_3v3_to_5v").in_range(1.0, 100).with_units(units.OHM),
)
@htf.plug(dmm=DMM)
def test_isolation_resistance(test, dmm):
    """Test isolation between power domains"""
    print("Testing isolation resistance...")

    test_pairs = [("3v3", "gnd"), ("5v", "gnd"), ("12v", "gnd"), ("3v3", "5v")]

    for rail1, rail2 in test_pairs:
        isolation = dmm.measure_isolation(rail1, rail2)
        key = f"isolation_{rail1}_to_{rail2}"
        setattr(test.measurements, key, isolation)
        print(f"Isolation {rail1.upper()}-{rail2.upper()}: {isolation:.1f}MΩ")


@htf.measures(
    htf.Measurement("nets_tested"),
    htf.Measurement("shorts_found").equals(0),
    htf.Measurement("opens_found").equals(0),
    htf.Measurement("shorts_test_passed").equals(True),
    htf.Measurement("opens_test_passed").equals(True),
)
@htf.plug(dmm=DMM)
def test_shorts_and_opens(test, dmm):
    """Check for shorts and open circuits"""
    print("Testing for shorts and opens...")

    critical_nets = ["uart_tx", "uart_rx", "spi_mosi", "spi_miso", "i2c_scl", "i2c_sda"]

    measurements.nets_tested = len(critical_nets)
    shorts = 0
    opens = 0

    for net in critical_nets:
        resistance = random.uniform(100, 500)  # kΩ
        continuity = random.choice([True] * 4 + [False])  # 80% pass

        if resistance < 10:
            shorts += 1
        if not continuity:
            opens += 1

    measurements.shorts_found = shorts
    measurements.opens_found = opens
    measurements.shorts_test_passed = shorts == 0
    measurements.opens_test_passed = opens == 0

    print(f"Shorts: {shorts}, Opens: {opens}")


@htf.measures(
    htf.Measurement("esd_usb_dp_vf").in_range(0.5, 0.8).with_units(units.Unit("V")),
    htf.Measurement("esd_usb_dp_leakage").in_range(0, 100).with_units(units.Unit("nA")),
    htf.Measurement("esd_usb_dn_vf").in_range(0.5, 0.8).with_units(units.Unit("V")),
    htf.Measurement("esd_usb_dn_leakage").in_range(0, 100).with_units(units.Unit("nA")),
)
@htf.plug(dmm=DMM)
def test_esd_protection(test, dmm):
    """Test ESD protection diodes"""
    print("Testing ESD protection...")

    protected_pins = ["usb_dp", "usb_dn"]

    for pin in protected_pins:
        forward_voltage = random.uniform(0.55, 0.75)
        leakage = random.uniform(1, 50)

        setattr(test.measurements, f"esd_{pin}_vf", forward_voltage)
        setattr(test.measurements, f"esd_{pin}_leakage", leakage)

        print(f"ESD {pin.upper()}: Vf={forward_voltage:.3f}V")


@htf.measures(
    htf.Measurement("mac_address"),
    htf.Measurement("oui").equals("AC:DE:48"),
    htf.Measurement("is_unicast").equals(True),
)
@htf.plug(dut=DUT)
def check_mac_address(test, dut):
    """Read and validate Ethernet MAC address"""
    print("Reading MAC address...")

    mac = dut.read_mac_address()
    measurements.mac_address = mac
    measurements.oui = mac[:8]

    first_byte = int(mac.split(":")[0], 16)
    measurements.is_unicast = (first_byte & 0x01) == 0

    print(f"MAC: {mac}")


@htf.measures(
    htf.Measurement("wifi_mac"),
    htf.Measurement("mac_difference").in_range(0, 10),
)
@htf.plug(dut=DUT)
def check_wifi_mac(test, dut):
    """Read and validate WiFi MAC address"""
    print("Reading WiFi MAC...")

    wifi_mac = dut.read_wifi_mac()
    eth_mac = dut.read_mac_address()

    measurements.wifi_mac = wifi_mac

    def mac_to_int(mac):
        return int(mac.replace(":", ""), 16)

    measurements.mac_difference = abs(mac_to_int(wifi_mac) - mac_to_int(eth_mac))

    print(f"WiFi MAC: {wifi_mac}")


@htf.measures(
    htf.Measurement("imei").equals(r"^\d{15}$"),
    htf.Measurement("imei_checksum_valid").equals(True),
)
@htf.plug(dut=DUT)
def check_imei(test, dut):
    """Read and validate IMEI number"""
    print("Reading IMEI...")

    imei = dut.read_imei()
    measurements.imei = imei

    # Luhn checksum
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

    measurements.imei_checksum_valid = luhn_check(imei)

    print(f"IMEI: {imei}")


# ==================== MAIN TEST ====================


def main():
    """Main test execution"""

    # Create test with metadata
    test = htf.Test(
        test_ain1,
        check_mac_address,
        check_wifi_mac,
        check_imei,
        test_power_rails,
        test_current_consumption,
        # test_analog_outputs,
        # test_digital_inputs,
        # test_digital_outputs,
        # test_pull_up_resistors,
        # test_isolation_resistance,
        # test_shorts_and_opens,
        # test_esd_protection,
        procedure_id="1cf7e59a-bf0a-11f0-b044-639a4e9293c8",
        part_number="IX01",
        revision="3.0.4",
        batch_number="1025-004",
    )

    # Execute test
    print("\n=== Starting IXrouter3 Hardware Test ===\n")
    with TofuPilot(test, api_key="6345e8d7-8f5d-4adb-98f2-6355a70fdc95"):
        test.execute(test_start=lambda: "001122334455")  # Serial number


if __name__ == "__main__":
    main()
