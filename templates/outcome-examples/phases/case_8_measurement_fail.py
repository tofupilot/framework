import time

def case_8_measurement_fail(measurements):
    time.sleep(0.5)

    voltage_reading = 2.5
    measurements.voltage = voltage_reading

    print(f"📊 Measured voltage: {voltage_reading}V (fails critical validator >= 3.0V)")
