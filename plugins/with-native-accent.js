const { AndroidConfig, withAndroidColors, withAndroidColorsNight, withAndroidStyles } = require('expo/config-plugins');

/** Match native dialogs/selection controls to the app without editing CNG output. */
function withNativeAccent(config) {
  config = withAndroidColors(config, mod => {
    mod.modResults = AndroidConfig.Colors.assignColorValue(mod.modResults, { name: 'meeterm_accent', value: '#85592E' });
    return mod;
  });
  config = withAndroidColorsNight(config, mod => {
    mod.modResults = AndroidConfig.Colors.assignColorValue(mod.modResults, { name: 'meeterm_accent', value: '#DBB378' });
    return mod;
  });
  return withAndroidStyles(config, mod => {
    for (const name of ['colorAccent', 'android:colorAccent']) {
      mod.modResults = AndroidConfig.Styles.assignStylesValue(mod.modResults, {
        add: true, parent: AndroidConfig.Styles.getAppThemeGroup(), name, value: '@color/meeterm_accent',
      });
    }
    return mod;
  });
}

module.exports = withNativeAccent;
