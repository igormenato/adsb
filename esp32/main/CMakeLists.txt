if(NOT (IDF_VERSION_MAJOR EQUAL 5 AND IDF_VERSION_MINOR EQUAL 5 AND IDF_VERSION_PATCH EQUAL 6))
    message(FATAL_ERROR "This firmware is pinned to ESP-IDF v5.5.6 (have ${IDF_VER})")
endif()

idf_component_register(SRCS "main.c" "trck_frame.c" INCLUDE_DIRS ".")
