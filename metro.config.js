const { getDefaultConfig } = require('expo/metro-config');

const config = getDefaultConfig(__dirname);

config.resolver.blockList = [
  ...config.resolver.blockList,
  /native[\\/]meeterm-core[\\/]target[\\/].*/,
  /modules[\\/]meeterm-terminal[\\/]android[\\/]build[\\/].*/,
];

module.exports = config;
