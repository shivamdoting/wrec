# Out-of-tree build of only KWin's screencast plugin (src/plugins/screencast)
# against the distro's installed libkwin 6.6.6 and kwin-dev headers. build.sh
# runs it; see README.md.
#
#   cmake -S <this dir> -B build -DKWIN_SOURCE_DIR=<unpacked kwin-6.6.6 tree>
#   nice -n19 cmake --build build -j2
#
# Compile flags, definitions and C++ standard copy kwin-6.6.6/CMakeLists.txt.
# Installed headers come first; the source tree is only a fallback for the two
# headers kwin-dev does not ship (core/syncobjtimeline.h, opengl/eglbackend.h)
# and what they include. Every installed non-generated header of Ubuntu's
# kwin-dev 4:6.6.6-0ubuntu0.1 is byte-identical to the 6.6.6 source.
cmake_minimum_required(VERSION 3.22)
project(kwin-screencast-r9 VERSION 6.6.6 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(KDE_COMPILERSETTINGS_LEVEL "5.82")
find_package(ECM 6.22.0 REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)
include(ECMQtDeclareLoggingCategory)
include(ECMDeprecationSettings)

find_package(Qt6 6.10.0 CONFIG REQUIRED COMPONENTS Core Gui Quick DBus Widgets)
find_package(KF6 6.22.0 REQUIRED COMPONENTS CoreAddons I18n Config WindowSystem)
find_package(KWin 6.6.6 EXACT CONFIG REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(PipeWire REQUIRED IMPORTED_TARGET libpipewire-0.3)

set(KWIN_SOURCE_DIR "" CACHE PATH "Unpacked kwin-6.6.6 source tree (with any patch applied)")
if(NOT EXISTS "${KWIN_SOURCE_DIR}/src/plugins/screencast/screencaststream.cpp")
    message(FATAL_ERROR "Set KWIN_SOURCE_DIR to the kwin-6.6.6 source tree")
endif()
set(SC ${KWIN_SOURCE_DIR}/src/plugins/screencast)

add_compile_options(-Wno-unused-parameter)
add_definitions(
    -DQT_NO_KEYWORDS
    -DQT_USE_QSTRINGBUILDER
    -DQT_NO_URL_CAST_FROM_STRING
    -DQT_NO_CAST_TO_ASCII
    -DQT_NO_FOREACH
    -DMESA_EGL_NO_X11_HEADERS
    -DEGL_NO_X11
    -DEGL_NO_PLATFORM_SPECIFIC_TYPES
)
ecm_set_disabled_deprecation_versions(QT 5.15 KF 6.9.0)

kcoreaddons_add_plugin(screencast INSTALL_NAMESPACE "kwin/plugins")
target_sources(screencast PRIVATE
    ${SC}/filteredsceneview.cpp
    ${SC}/main.cpp
    ${SC}/outputscreencastsource.cpp
    ${SC}/pipewirecore.cpp
    ${SC}/regionscreencastsource.cpp
    ${SC}/screencastbuffer.cpp
    ${SC}/screencastlayer.cpp
    ${SC}/screencastmanager.cpp
    ${SC}/screencastsource.cpp
    ${SC}/screencaststream.cpp
    ${SC}/windowscreencastsource.cpp
)
ecm_qt_declare_logging_category(screencast
    HEADER kwinscreencast_logging.h
    IDENTIFIER KWIN_SCREENCAST
    CATEGORY_NAME kwin_screencast
    DEFAULT_SEVERITY Warning
)
get_target_property(KWIN_INSTALLED_INCLUDES KWin::kwin INTERFACE_INCLUDE_DIRECTORIES)
target_include_directories(screencast PRIVATE ${SC} ${KWIN_INSTALLED_INCLUDES} ${KWIN_SOURCE_DIR}/src)
target_compile_definitions(screencast PRIVATE -DTRANSLATION_DOMAIN=\"kwin\")
target_link_libraries(screencast KWin::kwin KF6::I18n PkgConfig::PipeWire Libdrm::Libdrm)
