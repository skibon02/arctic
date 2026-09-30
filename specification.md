Polar Measurement Data Specification for 3rd Party

 •     <u>Abstract</u>
 •     <u>Introduction</u>
 •     <u>Gatt Service and Characteristics Declaration</u>
 •     <u>PMD Measurement Types</u>
 •     <u>Control Point Error Codes</u>
 •     <u>Frame types ACC</u>
 •     <u>Frame types Magnetometer</u>
 •     <u>Frame types Gyroscope</u>
 •     <u>Frame types PPG</u>
 •     <u>Frame types ECG</u>
 •     <u>Frame types PPI</u>
 •     <u>Delta frame sample example from Polar Verity Sense (Acc data as example)</u>
 •     <u>Prerequisite</u>
 •     <u>Read Features from device</u>
 •     <u>Request Stream Settings</u>
 •     <u>Start Stream</u>
 •     <u>Stop Stream</u>
 •     <u>Abbreviations</u>

Abstract

This document specifies BLE communication for Polar SDK. Reader should have a good
knowledge about BLE and GATT.

Introduction

This document is intending to explain the measurement data flow on top of Polar proprietary
Polar Measurement Data (PMD) service.

Gatt Service and Characteristics Declaration

Service          Characteristic Name              Property   Optional     Security     UUID
Name                                                         Property     Permission

PMD        NA                                                                          FB005C80-02E7-F387-
Service                                                                                1CAD-8ACD2D8DF0C8

           PMD Control Point                      Read,                                FB005C81-02E7-F387-
                                                  Write,                  None         1CAD-8ACD2D8DF0C8
                                                  Indicate

           PMD Control Point Client               Read,
           Characteristic Configuration           Write                   None
           Descriptor

           PMD Data MTU Characteristic            Notify     Indicate     None         FB005C82-02E7-F387-
                                                                                       1CAD-8ACD2D8DF0C8

           PMD Data MTU Client Characteristic     Read,                   None
           Configuration Descriptor               Write


PMD Measurement Types

     Measurement Types            Description                       Unit          Static Requirements

0                          ECG                        Volt (V)                    Lifetime
1                          PPG                                                    Lifetime

2                          Acceleration               Force per unit mass (g)     Lifetime

3                          PP Interval                Second (s)                  Lifetime

4                          Reserved for Future Use                                Lifetime


5    Gyroscope    Degrees per second (dps)  Lifetime


6         Magnetometer                Gauss (G)       Lifetime

7-255     Reserved for Future Use     Not defined     Not defined

Control Point Error Codes

Value               Description                                       Usage

0        SUCCESS                              Response when sent Control Point Command is handled
                                              with success.

1        ERROR INVALID OP CODE                Response when sent Control Point Command is not
                                              supported by device.

2        ERROR INVALID MEASUREMENT TYPE       Response when requested measurement is not known by
                                              the device.

3        ERROR NOT SUPPORTED                  Response when requested measurement is not supported
                                              by the device.

4        ERROR INVALID LENGTH                 Response when given length of doesn't match the
                                              received number of data.

5        ERROR INVALID PARAMETER              Response when request contains parameters that prevents
                                              handling the request.

6        ERROR ALREADY IN STATE               Response when device already in requested state.

7        ERROR INVALID RESOLUTION             Response when requested measurement with a resolution
                                              that is not supported by device.

8        ERROR INVALID SAMPLE RATE            Response when requested measurement with a sample
                                              rate that is not supported by device.

9        ERROR INVALID RANGE                  Response when requested measurement with a range that
                                              is not supported by device.

10       ERROR INVALID MTU                    Response when connection MTU is not matching the
                                              device required MTU.

11       ERROR INVALID NUMBER OF CHANNELS     Response when measurement request contains invalid
                                              number of channels.

12       ERROR INVALID STATE                  Response when device in invalid state.


13       ERROR DEVICE IN CHARGER     Response when device is in charger and doesn't support
                                     requested command in the current state.

14 -     RFU                         Reserved for Future Usage.
255

Frame types ACC

 Frame type     Size       Unit                  Description

 0              3B         mG      x, y, z 8-bit
 1              6B         mG      x, y, z 16-bit
 2              9B         mG      x, y, z 24-bit
 128            n          mG      Delta frame
 3..127,                           RFU
 129..255

Frame types Magnetometer

             Frame type            Size          Unit    Description

 128                               n     Gauss           Delta frame

 0..127, 129..255                                        RFU

Frame types Gyroscope

             Frame type               Size       Unit    Description

 128                               n            dps     Delta frame

 0..127, 129..255                                       RFU

Frame types PPG


Frame type    Size    Description


              Bytes 0..2: ppg0

              Bytes 3..5: ppg1
0       12B
              Bytes 6..8: ppg2

              Bytes 9..11: ambient0

128     n     Delta frame


1..127, 129..255    RFU


Frame types ECG

      Frame type          Size  Unit                Description

 0                3B            µV       Electrocardiogram
 1..255                                  RFU

Frame types PPI

      Frame type  Size                                        Description

                          Byte 0: Heart rate in bpm

                          Bytes 1..2: Peak to peak in milliseconds

 0                6B      Bytes 3..4: Error estimate
                          Byte 5: Flags.
                           bit0: error bit. If true then PP measurement is invalid for some reason.
                           bit1: skin contact status
                           bit2: skin contact status supported
 1..255                   RFU

Delta frame sample example from Polar Verity Sense
(Acc data as example)

 Index          Size                              Name                 Sample           Description
                                                                data hex
                                                                                  When resolution = 16bit and number of

 0        Requested measurement               Reference         D0 FF 65          channels = 3 then
          resolution * number of channels     sample            01 E4 0F          x channel: 0xFFD0 = -48, y channel:
                                                                                  0x0165 = 357, z channel: 0x0FE4 = 4068

 ..       1B                                  Delta size in     08                Each delta value is 8 bits in length
                                              bits

 ..       1B                                  Delta samples     1D                Samples count is 29.
                                              count
                                                                                  delta sample 0:

                                                                                  x channel: 0xFFD0 + 0xFC = 0xFFCC
                                                                                  => -52
 ..       Math.ceil((Delta size in bits *     Delta sample      FC 07 FF
          number of channels)/8)                                                  y channel: 0x0165 + 0x07 = 0x016C =>
                                                                                  364

                                                                                  z channel: 0x0FE4 + 0xFF = 0x0FE3 =>
                                                                                  4067
                                                                                  delta sample 1:

                                                                                  x channel: 0xFFCC + 0x0C = 0xFFD8
                                                                                  => -40
 ..       Math.ceil((Delta size in bits *     Delta sample      0C 13 F2
          number of channels)/8)                                                  y channel: 0x016C + 0x13 = 0x017F =>
                                                                                  383

                                                                                  z channel: 0x0FE3 + 0xF2 = 0x0FD5 =>
                                                                                  4053
 ..                                           Delta sample      ...               delta sample 2: ..

    Prerequisite


    Host    Device








    Connection established between host and device



    Switch to LE 2M PHY if possible



    Exchange ATT MTU with minimum 232



    Enable indication to PMD control point
    CCC Write

cCC Write response



    Enable notification to PMD Data MTU
    CCC Write

CCC Write response

    Read Features from device


    Host    Device








    Host reads PMD control point value to receive available features



   Read PMD control point, Polar Verity Sense response used as
                             example
              ATT CHARACTERISTIC READ

[OF 6E 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00]

            Ox0F = control point feature read response
    0x6E = PMD Measurement Types: bit0 ecg_supported=false, bit1
        ppg_supported=true, bit2 acc_supported=true, bit3
ppi_supported=true, bit4 rfu=false, bit5 gyro_supported=true, bit6
    mag_supported=true ...

    Request Stream Settings



    Host    Device






    Host requests acc stream settings



   Write to PMD control point, H10 response used as example
               Ox01 = Get measurement settings
                 0x02 = measurement_type(ACC)
                                     [01 02]

[F0 01 02 00 00 00 04 19 00 32 00 64 00 C8 00 01 01 10 00 02 03 02 00 04 00 08 00]

                OxF0 = control point response
           Ox01 = op_code(Get Measurement settings)
                 0x02 = measurement_type(ACC)
                  0x00 = error_code(success)
                  Ox00 = more_frames(false)
    Ox00 = setting_type(SAMPLE_RATE), Ox04 = array_length(4) , Ox19
0x00 = 25hz, 0x32 0x00 = 50hz, 0x64 0x00 = 100hz, 0xC8 0x00 =
                            200hz
0x01 = setting_type(RESOLUTION), Ox01 = array_length(1), Ox10
    0x00 = 16bit
0x02 = setting_type(RANGE), Ox03 = array_length(3) . 0x02 Ox00
             = 2G,0x04 0x00 = 4G, 0x08 0x00 = 8G


Write to PMD control point, Polar Verity Sense response used as
                            example
                Ox01 = Get measurement settings
                  0x02 = measurement_type(ACC)
                           [01 02]

[F0 01 02 00 00 00 01 34 00 01 01 10 00 02 01 08 00 04 01 03]

                 OxF0 = control point response
            Ox01 = op_code(Get Measurement settings)
                  0x02 = measurement_type(ACC)
                   0x00 = error_code(success)
    Ox00 = more_frames(false)
    0x00 = setting_type(SAMPLE_RATE), Ox01 = array_length(1) , Ox34
                          0x00 = 52hz
 0x01 = setting_type(RESOLUTION), Ox01 = array_length(1), Ox10
                          0x00 = 16bit
 0x02 = setting_type(RANGE), Ox01 = array_length(1) , Ox08 Ox00
  = 8G, 0x04 = setting_type(CHANNELS), Ox01 = array_length(1).
                       0x03 = 3 channels





    Host requests ecg stream settings




               Ox01 = Get measurement settings
                 0x00 = measurement_type(ECG)
                [01 00]

[F0 01 00 00 00 00 01 82 00 01 01 0E 00]

                OxF0 = control point response
           0x01 = op_code(Get Measurement settings)
                 Ox00 = measurement_type(ECG)
                  0x00 = error_code(success)
                  Ox00 = more_frames(false)
0x00 = setting_type(SAMPLE_RATE), Ox01 = array_length(1), 0x82
                         0x00 = 130hz
0x01 = setting_type(RESOLUTION), Ox01 = array_length(1), Ox0E
                         0x00 = 14bit





    Host requests ppg stream settings


   Write to PMD control point, OH1 response used as example
               Ox01 = Get measurement settings
                 0x01 = measurement_type(PPG)
                [01 01]

[F0 01 01 00 00 00 01 82 00 01 01 16 00]

                OxF0 = control point response
           Ox01 = op_code(Get Measurement settings)
                 0x01 = measurement_type(PPG)
                  0x00 = error_code(success)
                  Ox00 = more_frames(false)
Ox00 = setting_type(SAMPLE_RATE), Ox01 = array_length(1), Ox82
                         0x00 = 130hz
0x01 = setting_type(RESOLUTION), Ox01 = array_length(1), Ox16
                         0x00 = 22bit
                              20

    Start Stream


    Host    Device





    Host requests start acc stream



                   Ox02 = Start measurement
                 0x02 = measurement_type(ACC)
                              8G
Ox00 = setting_type(SAMPLE_RATE), Ox01 = array_length(1), OxC8
                         0x00 = 200hz
Ox01 = setting_type(RESOLUTION), Ox01 = array_length(1), Ox10
                         0x00 = 16bit
[02 02 02 01 08 00 00 01 C8 00 01 01 10 00]

            [F0 02 02 00 00 01]

                OxF0 = control point response
              Ox02 = op_code(Start Measurement)
                 0x02 = measurement_type(ACC)
                  0x00 = error_code(success)
                  Ox00 = more_frames(false)
                       0x01 = reserved




    Device starts stream to PMD Data characteristic
    [02 EA 54 A2 42 8B 45 52 08 01 45 FF E4 FF B5 03 45 FF E4 FF B8 03 ..]

   Ox02=ACC, [EA 54 A2 42 8B 45 52 08] = last sample timestamp in
                  nanoseconds (599618164814402794)
                        0x01 = ACC frameType
    sample0 = [45 FF E4 FF B5 03] x-axis(45 FF=-184 millig)
y-axis(E4 FF=-28 milig) z-axis(B5 03=949 milig) , sample1, sample2,



    Host requests start acc stream (delta framed)


 Write to PMD control point, Polar Verity Sense used as example
    Ox02 = Start measurement
                           0x02 = ACC
                          0x00 = 52hz
 0x01 = setting_type(RESOLUTION), Ox01 = array_length(1), 0x10
                          Ox00 = 16bit
 0x02 = setting_type(RANGE), 0x01 = array_length(1), 08 00 = 8G
 0x04 = setting_type(CHANNELS), 0x01 = array_length(1), Ox03 =
                           3 channels
[02 02 00 01 34 00 01 01 10 00 02 01 08 00 04 01 03]

         [F0 02 02 00 00 05 01 40 DA 7F 39]

                 OxF0 = control point response
               Ox02 = op_code(Start Measurement)
    0x02 = measurement_type(ACC)
                   0x00 = error_code(success)
Ox00 = more_frames(false) Ox05 = FACTOR, Ox01 = array_length(1)
    [40 DA 7F 39] = factor float IEEE754 value (0x397FDA40 =
                 964680256=0.000243999995291G)




    Device starts stream to PMD Data characteristic

    [02 64 6C 76 C4 3C C6 7F 07 80 D0 FF 65 01 E4 0F 08 1D FC 07 FF 0C 13 F2 11 F1 1A F9 EE F8 EF 0C 03 09 F2 ...]

                            0x02=ACC
      [64 6C 76 C4 3C C6 7F 07] = last sample timestamp in
                nanoseconds (540368444604181604)
                     0x80 = Delta frameType
    [D0 FF 65 01 E4 0F] = reference sample (-48, 357, 4068]
                Ox08 = delta size in bits(8bits)
                    0x1D = samples count(29)
[FC 07 FF] = delta sample 0 (-48-4 = -52, 357+7 = 364, 4068-1 =
                             4067)
    [0C 13 F2] = delta sample 1 (-52+12 = -40, 364+19 = 383.
                       4067-14= 4053) ..

    Stop Stream


    Host    Device









    Host stops ACC stream



Write to PMD control point, H10 used as example
            0x03 = Stop measurement
                   0x02 = ACC
    [03 02]

[F0 03 02 00 00]

         0xF0 = control point response
        0x03 = op_code(Stop Measurement)
          0x02 = measurement_type(ACC)
           0x00 = error_code(success)
           0x00 = more_frames(false)






    Host stops ECG stream



Write to PMD control point, H10 used as example
            0x03 = Stop measurement
                   0x00= ACC
    [03 00]

[F0 03 02 00 00]

         0xF0 = control point response
        0x03 = op_code(Stop Measurement)
          Ox00 = measurement_type(ECG)
           0x00 = error_code(success)
           0x00 = more_frames(false)

Abbreviations

<table>
  <thead>
    <tr>
      <th>Acronyms and Abbreviations</th>
      <th>Meaning</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td>PMD</td>
      <td>Polar Measurement Data</td>
    </tr>
    <tr>
      <td>BLE</td>
      <td>Bluetooth Low Energy</td>
    </tr>
    <tr>
      <td>GATT</td>
      <td>Generic Attribute Profile</td>
    </tr>
    <tr>
      <td>MTU</td>
      <td>Maximum Transmission Unit</td>
    </tr>
    <tr>
      <td>ACC</td>
      <td>Acceleration</td>
    </tr>
    <tr>
      <td>ECG</td>
      <td>Electrocardiogram</td>
    </tr>
    <tr>
      <td>PPG</td>
      <td>Photoplethysmogram</td>
    </tr>
    <tr>
      <td>PPI (or PP Interval)</td>
      <td>Peak-to-Peak interval</td>
    </tr>
    <tr>
      <td>dps</td>
      <td>degrees per second</td>
    </tr>
    <tr>
      <td>RFU</td>
      <td>Reserved for future use</td>
    </tr>
  </tbody>
</table>
