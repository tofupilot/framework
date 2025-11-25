def measure_output(log, measurements, power_supply):
    log.info("Measuring output voltage...")
    voltage = power_supply.read_voltage()
    measurements.output_voltage = voltage
    log.info(f"Measured: {voltage}V")
