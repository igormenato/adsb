Import("env")

env.BuildSources(
    "$BUILD_DIR/bench_common",
    "$PROJECT_DIR/../common",
    src_filter="+<*.c>",
)
