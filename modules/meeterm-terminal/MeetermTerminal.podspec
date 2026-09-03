Pod::Spec.new do |s|
  s.name             = 'MeetermTerminal'
  s.version          = '0.1.0'
  s.summary          = 'Native Metal terminal view backed by meeterm-core.'
  s.description      = 'Expo native terminal vertical slice with Rust-owned Alacritty state.'
  s.license          = { :type => 'MIT', :file => 'LICENSE' }
  s.author           = 'meeterm contributors'
  s.homepage         = 'https://github.com/phni3j9a/meeterm'
  s.source           = { :git => 'https://github.com/phni3j9a/meeterm.git', :tag => s.version.to_s }
  s.platforms        = { :ios => '16.4' }
  s.swift_version    = '5.9'
  s.static_framework = true

  s.source_files = 'ios/**/*.swift'
  s.preserve_paths = [
    'ios/build-rust.sh',
    'ios/include/**/*'
  ]
  s.frameworks = 'CoreGraphics', 'CoreText', 'Metal', 'MetalKit', 'UIKit'
  s.libraries = 'meeterm_core'
  s.dependency 'ExpoModulesCore'

  s.script_phase = {
    :name => 'Build meeterm-core for iOS',
    :script => '/bin/bash "${PODS_TARGET_SRCROOT}/ios/build-rust.sh"',
    :execution_position => :before_compile,
    :input_files => [
      '${PODS_TARGET_SRCROOT}/../../native/meeterm-core/Cargo.toml',
      '${PODS_TARGET_SRCROOT}/../../native/meeterm-core/Cargo.lock',
      '${PODS_TARGET_SRCROOT}/../../native/meeterm-core/rust-toolchain.toml',
      '${PODS_TARGET_SRCROOT}/../../native/meeterm-core/src'
    ]
  }

  # The custom Clang module makes the C ABI visible to this pod's Swift source.
  # The user target receives only a search path; `s.libraries` supplies -l.
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    'ENABLE_USER_SCRIPT_SANDBOXING' => 'NO',
    'HEADER_SEARCH_PATHS' => '$(inherited) "$(PODS_TARGET_SRCROOT)/ios/include"',
    'LIBRARY_SEARCH_PATHS' => '$(inherited) "$(BUILT_PRODUCTS_DIR)"',
    'SWIFT_INCLUDE_PATHS' => '$(inherited) "$(PODS_TARGET_SRCROOT)/ios/include"'
  }
  s.user_target_xcconfig = {
    'LIBRARY_SEARCH_PATHS' => '$(inherited) "$(BUILT_PRODUCTS_DIR)"'
  }
end
