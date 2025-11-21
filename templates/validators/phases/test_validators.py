def test_multiple_lower_bounds(measurements, log):
    log.info("Testing multiple > validators at different severity levels")
    temp = 25.5
    measurements.temperature = temp
    log.info(f"Temperature: {measurements.temperature}°C (should pass all validators)")


def test_multiple_upper_bounds(measurements, log):
    log.info("Testing multiple < validators at different severity levels")
    measurements.pressure = 50.0
    log.info(f"Pressure: {measurements.pressure} kPa (should pass all validators)")


def test_range_with_extra(measurements, log):
    log.info("Testing range with inequality constraint")
    measurements.voltage = 4.0
    log.info(f"Voltage: {measurements.voltage}V (in range, satisfies != 4.2)")


def test_numeric_equality(measurements, log):
    log.info("Testing numeric == operator")
    measurements.exact_count = 42
    log.info(f"Exact count: {measurements.exact_count} (should equal 42)")


def test_numeric_inequality(measurements, log):
    log.info("Testing numeric != operator")
    measurements.error_code = 1
    log.info(f"Error code: {measurements.error_code} (should not equal 0)")


def test_string_equality(measurements, log):
    log.info("Testing string == operator")
    measurements.status = "PASS"
    log.info(f"Status: {measurements.status} (should equal 'PASS')")


def test_string_inequality(measurements, log):
    log.info("Testing string != operator")
    measurements.error_message = "OK"
    log.info(
        f"Error message: {measurements.error_message} (should not equal 'FAILED')"
    )


def test_string_pattern(measurements, log):
    log.info("Testing string matches operator")
    measurements.serial_number = "SN-123456"
    log.info(f"Serial number: {measurements.serial_number} (should match pattern)")


def test_string_contains(measurements, log):
    log.info("Testing string contains operator")
    measurements.log_message = "Operation completed successfully"
    log.info(f"Log message: {measurements.log_message} (should contain 'success')")


def test_boolean_equality(measurements, log):
    log.info("Testing boolean == operator")
    measurements.system_ready = True
    log.info(f"System ready: {measurements.system_ready} (should equal True)")


def test_boolean_inequality(measurements, log):
    log.info("Testing boolean != operator")
    measurements.has_error = False
    log.info(f"Has error: {measurements.has_error} (should not equal True)")


def test_expression_validator(measurements, log):
    log.info("Testing expression-based validator")
    measurements.outlier_detection = 50.0
    log.info(f"Outlier detection: {measurements.outlier_detection}")


def test_mixed_validators(measurements, log):
    log.info("Testing mixed standard and expression validators")
    measurements.sensor_reading = 50.0
    log.info(
        f"Sensor reading: {measurements.sensor_reading} (should satisfy all validators)"
    )


def test_complex_range(measurements, log):
    log.info("Testing complex range with multiple overlapping validators")
    measurements.complex_measurement = 50.0
    log.info(
        f"Complex measurement: {measurements.complex_measurement} (should satisfy all range constraints)"
    )
