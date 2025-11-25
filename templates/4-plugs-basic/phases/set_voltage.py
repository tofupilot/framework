def set_voltage(log, power_supply):
    log.info("Configuring power supply...")
    power_supply.set_voltage(5.0)
    log.info("Power supply configured")
