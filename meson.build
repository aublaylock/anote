project('rnote', ['rust', 'cpp'], version: '0.15.0', meson_version: '>= 1.0')
# add a patch suffix for alpha or beta versions in format '-<alpha|beta>.<x>'.
patch = ''

# Modules
gnome = import('gnome')

# Options
prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
libdir = prefix / get_option('libdir')
localedir = prefix / get_option('localedir')
sysconfdir = prefix / get_option('sysconfdir')
profile = get_option('profile')
build_ui = get_option('ui')
build_cli = get_option('cli')
ci = get_option('ci')

app_name = 'rnote'
app_name_capitalized = 'Rnote'
cli_name = 'rnote-cli'
base_id = 'com.github.flxzt.' + app_name
app_website = 'https://rnote.flxzt.net'
app_repo_url = 'https://github.com/flxzt/rnote'
app_issues_url = 'https://github.com/flxzt/rnote/issues'
app_support_url = 'https://github.com/flxzt/rnote/discussions'
app_donate_url = 'https://rnote.flxzt.net/#support'
app_author_name = 'Felix Zwettler'
app_authors = run_command(
    'cat',
    meson.project_source_root() / 'AUTHORS',
    check: true,
).stdout().strip()

version = meson.project_version()
version_array = version.split('.')
version_major = version_array[0].to_int()
version_minor = version_array[1].to_int()
version_micro = version_array[2].to_int()

pkgdatadir = datadir / app_name
iconsdir = datadir / 'icons'
gettext_package = app_name
app_idpath = '/com/github/flxzt/' + app_name + '/'

if profile == 'devel'
    app_id = base_id + '.' + 'Devel'
    version_suffix = '-devel'
    rust_target_folder = 'debug'
else
    app_id = base_id
    version_suffix = patch
    rust_target_folder = 'release'
endif

# Dependencies
dependency('glib-2.0', version: '>= 2.88')
dependency('gio-2.0', version: '>= 2.88')
dependency('cairo', version: '>= 1.18')
# UI/CLI specific dependencies are declared below

cargo = find_program('cargo', required: true)
# cmake is needed by the "ink-stroke-modeler-rs" bindings crate to build the C++ library
find_program('cmake', required: true)
find_program('glib-compile-resources', required: true)
cargo_nextest = find_program('cargo-nextest', required: false)
glib_compile_schemas = find_program('glib-compile-schemas', required: true)
desktop_file_validate = find_program('desktop-file-validate', required: false)
appstreamcli = find_program('appstreamcli', required: false)
find_program('fc-cache', required: false)
find_program('update-mime-database', required: false)
cargo_build_script = find_program('build-aux/cargo_build.py', required: true)

cargo_env = ['CARGO_HOME=' + meson.project_build_root() / 'cargo-home']
cargo_manifest_path = meson.project_source_root() / 'Cargo.toml'
cargo_target_dir = meson.project_build_root() / 'target'

meson.add_dist_script(
    'build-aux/dist-vendor.sh',
    meson.project_build_root() / 'meson-dist' / app_name + '-' + version,
    meson.project_source_root(),
)

# core crates
subdir('crates/rnote-compose')
subdir('crates/rnote-engine')

# cargo fmt check
run_target(
    'cargo-fmt-check',
    command: [
        cargo,
        ['fmt', '--all', '--check', '--manifest-path', cargo_manifest_path],
    ],
    env: cargo_env,
)

# cargo tests
if cargo_nextest.found()
    nextest_args = [
        'nextest',
        '--manifest-path',
        cargo_manifest_path,
        'run',
        '--workspace',
        '--target-dir',
        cargo_target_dir,
    ]
    if ci == true
        nextest_args += ['--profile', 'ci']
    endif
    run_target('cargo-test', command: [cargo, nextest_args], env: cargo_env)
endif

# cargo clean
run_target(
    'cargo-clean',
    command: [
        cargo,
        [
            'clean',
            '--manifest-path',
            cargo_manifest_path,
            '--target-dir',
            cargo_target_dir,
        ],
    ],
    env: cargo_env,
)

# build/install the ui
if build_ui == true
    # ui dependencies
    dependency('appstream', version: '>=1.0')
    dependency('gtk4', version: '>= 4.22')
    dependency('libadwaita-1', version: '>= 1.9')

    # ui crate
    subdir('crates/rnote-ui')

    ui_sources = [
        rnote_compose_sources,
        rnote_engine_sources,
        rnote_ui_sources,
        rnote_ui_data_files,
        files(
            meson.project_source_root() + '/crates/rnote-compose/Cargo.toml',
            meson.project_source_root() + '/crates/rnote-engine/Cargo.toml',
            meson.project_source_root() + '/crates/rnote-ui/Cargo.toml',
            meson.project_source_root() + '/crates/rnote-ui/build.rs',
            meson.project_source_root() + '/Cargo.lock',
            meson.project_source_root() + '/Cargo.toml',
        ),
    ]

    ui_cargo_options = ['--manifest-path', cargo_manifest_path]
    ui_cargo_options += ['--target-dir', cargo_target_dir]
    ui_cargo_options += ['-p', 'rnote']

    if profile == 'default'
        ui_cargo_options += ['--release']
    endif

    ui_output = app_name

    # ui cargo check
    run_target(
        'ui-cargo-check',
        command: [cargo, 'check', ui_cargo_options],
        env: cargo_env,
    )

    # ui cargo clippy
    run_target(
        'ui-cargo-clippy',
        command: [cargo, 'clippy', ui_cargo_options],
        env: cargo_env,
    )

    # ui cargo doc
    run_target(
        'ui-cargo-doc',
        command: [cargo, 'doc', ui_cargo_options],
        env: cargo_env,
    )

    # ui cargo build
    app_cargo_build = custom_target(
        'ui-cargo-build',
        build_by_default: true,
        input: ui_sources,
        output: ui_output,
        console: true,
        install: true,
        install_dir: bindir,
        command: [
            cargo_build_script,
            meson.project_build_root(),
            meson.project_source_root(),
            ' '.join(cargo_env),
            cargo,
            ' '.join(ui_cargo_options),
            cargo_target_dir / rust_target_folder / app_name,
            meson.project_build_root() / '@OUTPUT@',
        ],
    )

    meson.add_install_script(
        'build-aux/meson_post_install.py',
        datadir,
        bindir,
        app_name,
    )

    gnome.post_install(
        glib_compile_schemas: true,
        gtk_update_icon_cache: true,
        update_desktop_database: true,
        update_mime_database: true,
    )
endif

# build/install the cli
if build_cli == true
    # cli crate
    subdir('crates/rnote-cli')

    cli_cargo_options = ['--manifest-path', cargo_manifest_path]
    cli_cargo_options += ['--target-dir', cargo_target_dir]
    cli_cargo_options += ['-p', 'rnote-cli']

    if profile == 'default'
        cli_cargo_options += ['--release']
    endif

    cli_output = cli_name

    cli_sources = [
        rnote_compose_sources,
        rnote_engine_sources,
        rnote_cli_sources,
        files(
            meson.project_source_root() + '/crates/rnote-compose/Cargo.toml',
            meson.project_source_root() + '/crates/rnote-engine/Cargo.toml',
            meson.project_source_root() + '/crates/rnote-cli/Cargo.toml',
            meson.project_source_root() + '/Cargo.lock',
            meson.project_source_root() + '/Cargo.toml',
        ),
    ]

    # cli cargo check
    run_target(
        'cli-cargo-check',
        command: [cargo, 'check', cli_cargo_options],
        env: cargo_env,
    )

    # cli cargo clippy
    run_target(
        'cli-cargo-clippy',
        command: [cargo, 'clippy', cli_cargo_options],
        env: cargo_env,
    )

    # cli cargo doc
    run_target(
        'cli-cargo-doc',
        command: [cargo, 'doc', cli_cargo_options],
        env: cargo_env,
    )

    # cli cargo build
    custom_target(
        'cli-cargo-build',
        build_by_default: true,
        input: cli_sources,
        output: cli_output,
        console: true,
        install: true,
        install_dir: bindir,
        command: [
            cargo_build_script,
            meson.project_build_root(),
            meson.project_source_root(),
            cargo_env,
            cargo,
            ' '.join(cli_cargo_options),
            cargo_target_dir / rust_target_folder / cli_name,
            meson.project_build_root() / '@OUTPUT@',
        ],
    )
endif
