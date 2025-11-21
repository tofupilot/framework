import random
import statistics
import re


def test_power_rails(measurements):
    """Test all power supply voltages"""
    print("Testing power rails...")

    # 3.3V rail
    voltage_3v3 = [random.uniform(3.25, 3.35) for _ in range(10)]
    measurements.voltage_3v3_mean = statistics.mean(voltage_3v3)
    measurements.voltage_3v3_ripple = max(voltage_3v3) - min(voltage_3v3)

    # 5V rail
    voltage_5v = [random.uniform(4.95, 5.05) for _ in range(10)]
    measurements.voltage_5v_mean = statistics.mean(voltage_5v)
    measurements.voltage_5v_ripple = max(voltage_5v) - min(voltage_5v)

    # 12V rail
    voltage_12v = [random.uniform(11.9, 12.1) for _ in range(10)]
    measurements.voltage_12v_mean = statistics.mean(voltage_12v)
    measurements.voltage_12v_ripple = max(voltage_12v) - min(voltage_12v)

    print(
        f"3.3V rail: {measurements.voltage_3v3_mean:.3f}V (ripple: {measurements.voltage_3v3_ripple:.3f}V)"
    )
    print(
        f"5V rail: {measurements.voltage_5v_mean:.3f}V (ripple: {measurements.voltage_5v_ripple:.3f}V)"
    )
    print(
        f"12V rail: {measurements.voltage_12v_mean:.3f}V (ripple: {measurements.voltage_12v_ripple:.3f}V)"
    )


def test_current_consumption(measurements):
    """Test current draw on each power rail"""
    print("Measuring current consumption...")

    # Idle current (no load)
    measurements.current_3v3_idle = random.uniform(45, 55)  # mA
    measurements.current_5v_idle = random.uniform(20, 30)  # mA
    measurements.current_12v_idle = random.uniform(5, 10)  # mA

    # Active current (under load)
    measurements.current_3v3_active = random.uniform(180, 220)  # mA
    measurements.current_5v_active = random.uniform(80, 120)  # mA
    measurements.current_12v_active = random.uniform(150, 200)  # mA

    # Calculate total power
    measurements.power_idle = (
        measurements.current_3v3_idle * 3.3
        + measurements.current_5v_idle * 5.0
        + measurements.current_12v_idle * 12.0
    )  # mW

    measurements.power_active = (
        measurements.current_3v3_active * 3.3
        + measurements.current_5v_active * 5.0
        + measurements.current_12v_active * 12.0
    )  # mW

    print(f"Idle power: {measurements.power_idle:.1f}mW")
    print(f"Active power: {measurements.power_active:.1f}mW")


def test_analog_inputs(measurements):
    """Test analog input channels"""
    print("Testing analog inputs...")

    # Test 4 analog input channels (0-10V range)
    for channel in range(4):
        # Apply known test voltage (simulate with random in range)
        test_voltage = 5.0  # Applied voltage
        readings = [random.uniform(4.95, 5.05) for _ in range(10)]

        mean_reading = statistics.mean(readings)
        error = abs(mean_reading - test_voltage)

        setattr(measurements, f"ain{channel}_voltage", mean_reading)
        setattr(measurements, f"ain{channel}_error", error)
        setattr(
            measurements, f"ain{channel}_error_percent", (error / test_voltage) * 100
        )

        print(f"AIN{channel}: {mean_reading:.3f}V (error: {error*1000:.1f}mV)")
