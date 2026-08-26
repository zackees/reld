cmake_minimum_required(VERSION 3.20)
project(reld_cpp_name_mangling LANGUAGES CXX)

enable_testing()

add_library(reld_mangled_symbols STATIC src/mangled.cpp)
target_compile_features(reld_mangled_symbols PUBLIC cxx_std_17)
target_include_directories(reld_mangled_symbols PUBLIC include)

add_executable(reld_cpp_name_mangling src/main.cpp)
target_link_libraries(reld_cpp_name_mangling PRIVATE reld_mangled_symbols)

add_test(NAME reld_cpp_name_mangling COMMAND reld_cpp_name_mangling)
