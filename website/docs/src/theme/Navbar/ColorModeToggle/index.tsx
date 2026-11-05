/**
 * Swizzled to make the toggle a plain light/dark switch.
 *
 * The stock navbar toggle cycles `system -> light -> dark`, so on a machine
 * whose system preference already is light the first click only pins the
 * colour that is already showing and nothing changes. Here the effective
 * colour is the switch value and every click flips it, while a first visit
 * still follows the system preference until the reader chooses.
 */
import React from 'react';
import {useColorMode, useThemeConfig} from '@docusaurus/theme-common';
import ColorModeToggle from '@theme/ColorModeToggle';

export default function NavbarColorModeToggle({
  className,
}: {
  className?: string;
}) {
  const {disableSwitch} = useThemeConfig().colorMode;
  const {colorMode, setColorMode} = useColorMode();

  if (disableSwitch) {
    return null;
  }

  return (
    <ColorModeToggle
      className={className}
      respectPrefersColorScheme={false}
      value={colorMode}
      onChange={setColorMode}
    />
  );
}
