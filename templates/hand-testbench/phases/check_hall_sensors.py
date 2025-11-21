def check_hall_sensors(phase, measurements, ethercat):
    hall_empty = ethercat.read_hall_sensors()
    top_empty_hall = hall_empty["top"]
    bot_empty_hall = hall_empty["bot"]

    hall_handle = ethercat.read_hall_sensors()
    top_handle_hall = hall_handle["top"]
    bot_handle_hall = hall_handle["bot"]

    measurements.top_magnetic_field_delta_x = abs(top_empty_hall[0] - top_handle_hall[0])
    measurements.top_magnetic_field_delta_y = abs(top_empty_hall[1] - top_handle_hall[1])
    measurements.top_magnetic_field_delta_z = abs(top_empty_hall[2] - top_handle_hall[2])
    measurements.top_check_order_x = top_handle_hall[0] - top_empty_hall[0]
    measurements.top_check_order_y = top_handle_hall[1] - top_empty_hall[1]
    measurements.top_check_order_z = top_handle_hall[2] - top_empty_hall[2]

    measurements.bot_magnetic_field_delta_x = abs(bot_empty_hall[0] - bot_handle_hall[0])
    measurements.bot_magnetic_field_delta_y = abs(bot_empty_hall[1] - bot_handle_hall[1])
    measurements.bot_magnetic_field_delta_z = abs(bot_empty_hall[2] - bot_handle_hall[2])
    measurements.bot_check_order_x = bot_handle_hall[0] - bot_empty_hall[0]
    measurements.bot_check_order_y = bot_handle_hall[1] - bot_empty_hall[1]
    measurements.bot_check_order_z = bot_handle_hall[2] - bot_empty_hall[2]

    measurements.temperature_range_test = 20.5

    
