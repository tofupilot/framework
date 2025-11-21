def default_value_pattern(run):
    log.info("Pre-filled value with pattern validation")
    log.info("Default value: 'SN-'")
    log.info("Pattern: ^SN-[0-9]{8}$")
    log.info("")
    log.info("This helps guide the operator by providing a template")
    log.info("The operator only needs to complete the 8 digits")
    log.info("Useful for standardized formats where prefix is fixed")
