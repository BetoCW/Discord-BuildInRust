# Changelog

Formato basado en [Keep a Changelog](https://keepachangelog.com/es/).

## [0.3.0] — 2026-06-15 — Optimización de sonido

Release centrado en **calidad de audio y resiliencia de paquetes** (la RAM ya era
~22 MB vs 300–800 MB del cliente oficial). TX/RX de voz **validados en vivo**.

### Añadido
- **Jitter buffer por emisor (RX)**: reordena los paquetes Opus por número de
  secuencia RTP y mantiene un cojín (~60 ms) para absorber el desorden de UDP, que
  antes entrecortaba el audio.
- **PLC + FEC**: los paquetes perdidos se rellenan con la ocultación de pérdida de
  Opus (PLC) o se reconstruyen desde el FEC in-band del paquete siguiente. Nuevo
  contador `concealados` en el log.
- **Opus afinado (TX)**: FEC in-band, `packet_loss_perc=10` y bitrate 64 kbps para
  resistir la pérdida de paquetes.
- **Remuestreo cúbico** (puro Rust, sin dependencias): permite micrófonos/altavoces
  que no funcionan a 48 kHz (antes la voz salía a tono incorrecto). La ruta de 48 kHz
  queda idéntica.
- **Modos de supresión de ruido estilo Discord** en «Ajustes de voz»: Desactivada /
  Ligera (puerta de ruido) / Aislamiento de voz (RNNoise por IA).
- **Cancelación de eco avanzada (AEC) — experimental, opt-in**: filtro adaptativo
  NLMS que *resta* el eco (permite doble-habla) en vez de solo atenuar. Desactivado
  por defecto; el ducker básico sigue siendo el predeterminado.
- **Metadatos de versión** incrustados en el `.exe` (VERSIONINFO) para reducir
  falsos positivos de antivirus.

### Cambiado
- **RNNoise a mono**: la supresión de ruido neuronal procesa un solo canal (la voz
  es mono) en vez de dos idénticos → ~la mitad del coste de CPU del paso más caro.

### Corregido
- **Concealment inflado por comfort-noise**: los frames de silencio del emisor
  avanzaban el número de secuencia pero se descartaban, y el jitter buffer los veía
  como pérdidas y fabricaba audio con PLC. Ahora se marcan como silencio y solo
  mantienen la continuidad de la secuencia.

### Notas
- Los avisos de "virus" en algunas máquinas son **falsos positivos** (binario sin
  firmar + el importador de token lee credenciales locales de Discord, patrón que los
  antivirus marcan). Ver README → «Falsos positivos de antivirus».

## [0.2.1] — 2026-06-10
- Arreglada la voz robotizada: procesado del micrófono suave y opt-in.

## [0.2.0] — 2026-06-10
- Ajustes de voz estilo Discord (anti-eco, volúmenes, prueba de micrófono),
  icono nuevo e instalador (Inno Setup).
