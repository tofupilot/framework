def check_proximity_sensors(phase, measurements, ethercat):
    proximity_empty = ethercat.read_proximity_sensors()
    top_empty_proximity = proximity_empty["top"]
    bot_empty_proximity = proximity_empty["bot"]

    ethercat.close_fingers(3.0)

    proximity_handle = ethercat.read_proximity_sensors()
    top_handle_proximity = proximity_handle["top"]
    bot_handle_proximity = proximity_handle["bot"]

    ethercat.open_fingers(3.0)

    measurements.top_force_delta = abs(top_empty_proximity - top_handle_proximity)
    measurements.top_check_order = top_handle_proximity - top_empty_proximity

    measurements.bot_force_delta = abs(bot_empty_proximity - bot_handle_proximity)
    measurements.bot_check_order = bot_handle_proximity - bot_empty_proximity

    
