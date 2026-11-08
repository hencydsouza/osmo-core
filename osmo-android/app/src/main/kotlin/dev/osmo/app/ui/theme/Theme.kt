package dev.osmo.app.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

// Palette: my design decision (not in M3). Signal orange on warm ink, like a camera's record light,
// with a teal secondary for "connected/ok" and a yellow tertiary kept for stars.
// Fixed brand colours instead of wallpaper-derived dynamic colour so the app looks the same on every phone.
private val Light = lightColorScheme(
    primary = Color(0xFFA13F00), onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFFFDBC9), onPrimaryContainer = Color(0xFF351000),
    secondary = Color(0xFF00695F), onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFB4F0E6), onSecondaryContainer = Color(0xFF00201C),
    tertiary = Color(0xFF745B00), onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFFFE08A), onTertiaryContainer = Color(0xFF241A00),
    error = Color(0xFFBA1A1A), onError = Color(0xFFFFFFFF),
    background = Color(0xFFFFF8F5), onBackground = Color(0xFF221A15),
    surface = Color(0xFFFFF8F5), onSurface = Color(0xFF221A15),
    surfaceVariant = Color(0xFFF4DED4), onSurfaceVariant = Color(0xFF52443C),
    surfaceContainerLowest = Color(0xFFFFFFFF), surfaceContainerLow = Color(0xFFFFF1EA),
    surfaceContainer = Color(0xFFFBEBE2), surfaceContainerHigh = Color(0xFFF5E5DC),
    surfaceContainerHighest = Color(0xFFEFDFD6),
    outline = Color(0xFF85736B), outlineVariant = Color(0xFFD8C2B8),
)

private val Dark = darkColorScheme(
    primary = Color(0xFFFFB68F), onPrimary = Color(0xFF552100),
    primaryContainer = Color(0xFF7A3300), onPrimaryContainer = Color(0xFFFFDBC9),
    secondary = Color(0xFF7FD6C9), onSecondary = Color(0xFF003731),
    secondaryContainer = Color(0xFF005048), onSecondaryContainer = Color(0xFFB4F0E6),
    tertiary = Color(0xFFF2C94C), onTertiary = Color(0xFF3D2F00),
    tertiaryContainer = Color(0xFF584400), onTertiaryContainer = Color(0xFFFFE08A),
    error = Color(0xFFFFB4AB), onError = Color(0xFF690005),
    background = Color(0xFF17120F), onBackground = Color(0xFFEDE0D9),
    surface = Color(0xFF17120F), onSurface = Color(0xFFEDE0D9),
    surfaceVariant = Color(0xFF52443C), onSurfaceVariant = Color(0xFFD8C2B8),
    surfaceContainerLowest = Color(0xFF110C0A), surfaceContainerLow = Color(0xFF1F1A17),
    surfaceContainer = Color(0xFF241E1A), surfaceContainerHigh = Color(0xFF2F2824),
    surfaceContainerHighest = Color(0xFF3A332E),
    outline = Color(0xFFA08D84), outlineVariant = Color(0xFF52443C),
)

/** Heavier, tighter headlines: M3 Expressive "emphasized" type (weight carries hierarchy). */
private fun emphasized(base: TextStyle, weight: FontWeight = FontWeight.Bold) = base.copy(fontWeight = weight)

@Composable
private fun osmoTypography(): Typography {
    val t = MaterialTheme.typography
    return t.copy(
        displaySmall = emphasized(t.displaySmall, FontWeight.ExtraBold).copy(letterSpacing = (-0.5).sp),
        headlineLarge = emphasized(t.headlineLarge, FontWeight.ExtraBold),
        headlineMedium = emphasized(t.headlineMedium),
        titleLarge = emphasized(t.titleLarge, FontWeight.SemiBold),
        titleMedium = emphasized(t.titleMedium, FontWeight.SemiBold),
    )
}

/** M3 Expressive: expressive motion springs, brand colour. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun OsmoTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    MaterialExpressiveTheme(
        colorScheme = if (dark) Dark else Light,
        motionScheme = MotionScheme.expressive(),
    ) {
        MaterialTheme(typography = osmoTypography(), content = content)
    }
}
