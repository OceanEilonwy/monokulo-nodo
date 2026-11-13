// SPDX-License-Identifier: GPL-2.0
/*
 * ZHOT ZH055OE041 5.5" 1080x1920 AMOLED MIPI-DSI panel (Raydium RM67199-class controller),
 * the built-in screen of the NanoPC-T6N based Nodo.
 *
 * Mainline's panel-raydium-rm67191 only knows the NXP RM67191 panel. This driver carries the
 * ZH055OE041 support from FriendlyElec's panel-raydium-rm67191.c (kernel-rockchip, nanopi6-v6.1.y
 * at commit 30d3e1c2470a7282f37b96fdd4354314dda69aa3): its manufacturer command table
 * (mcs_zh055oe), its power-on sequence (rm67199_enable) and the display timing from FriendlyElec's
 * rk3588-nanopi6-mipi-55amoled.dtsi, written for mainline's panel and DSI APIs. The pixel clock is
 * 132 MHz, not FriendlyElec's 133.711 MHz: the RK3588 makes 132 MHz exactly for VOP2's DSI video
 * port, and when the clock it gives differs from the mode, mainline's DSI host times each line for
 * the wrong rate. The panel's 4-pixel hsync also needs VOP2's pre-scan hblank clamped to 8
 * (patches/0001-drm-rockchip-vop2-pre-scan-hblank-minimum.patch), or the picture jitters.
 *
 * Based on panel-raydium-rm67191.c, Copyright 2019 NXP.
 */

#include <linux/backlight.h>
#include <linux/delay.h>
#include <linux/gpio/consumer.h>
#include <linux/media-bus-format.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/regulator/consumer.h>

#include <video/mipi_display.h>

#include <drm/drm_crtc.h>
#include <drm/drm_mipi_dsi.h>
#include <drm/drm_modes.h>
#include <drm/drm_panel.h>

/* Panel specific color-format bits */
#define COL_FMT_16BPP 0x55
#define COL_FMT_18BPP 0x66
#define COL_FMT_24BPP 0x77

/* Write Manufacture Command Set Control */
#define WRMAUCCTR 0xFE

struct cmd_set_entry {
	u8 cmd;
	u8 param;
};

/* From FriendlyElec, received by them from the panel vendor; used as is. */
static const struct cmd_set_entry mcs_zh055oe[] = {
	{0xFE, 0xA0}, {0x2B, 0x18}, {0xFE, 0x70}, {0x7D, 0x35},
	{0x5D, 0x0A}, {0x5A, 0x79}, {0x5C, 0x00}, {0x52, 0x00},
	{0xFE, 0xD0}, {0x40, 0x02}, {0x13, 0x40}, {0xFE, 0x40},
	{0x05, 0x08}, {0x06, 0x08}, {0x08, 0x08}, {0x09, 0x08},
	{0x0A, 0xCA}, {0x0B, 0x88}, {0x20, 0x93}, {0x21, 0x93},
	{0x24, 0x02}, {0x26, 0x02}, {0x28, 0x05}, {0x2A, 0x05},
	{0x74, 0x2F}, {0x75, 0x1E}, {0xAD, 0x00}, {0xFE, 0x50},
	{0xDA, 0x01}, {0x44, 0x05}, {0x42, 0x05}, {0x40, 0x05},
	{0x3A, 0x05}, {0x38, 0x05}, {0x36, 0x05}, {0x34, 0x05},
	{0x32, 0x05}, {0x30, 0x05}, {0x2A, 0x05}, {0x28, 0x05},
	{0x26, 0x04}, {0x24, 0x04}, {0x22, 0x04}, {0x20, 0x04},
	{0x1E, 0x04}, {0x1C, 0x04}, {0x1A, 0x04}, {0x18, 0x03},
	{0x16, 0x03}, {0x14, 0x03}, {0x12, 0x03}, {0x10, 0x03},
	{0x0E, 0x03}, {0x0C, 0x03}, {0x0A, 0x02}, {0x08, 0x02},
	{0x06, 0x02}, {0x04, 0x02}, {0x02, 0x03}, {0xD0, 0x02},
	{0xCE, 0x02}, {0x00, 0x00}, {0x45, 0xA7}, {0x43, 0xA0},
	{0x41, 0x99}, {0x3B, 0x8C}, {0x39, 0x80}, {0x37, 0x6A},
	{0x35, 0x54}, {0x33, 0x3F}, {0x31, 0x2A}, {0x2B, 0x15},
	{0x29, 0x01}, {0x27, 0xED}, {0x25, 0xD9}, {0x23, 0xB0},
	{0x21, 0x89}, {0x1F, 0x62}, {0x1D, 0x3A}, {0x1B, 0x11},
	{0x19, 0xE9}, {0x17, 0xBF}, {0x15, 0x8B}, {0x13, 0x71},
	{0x11, 0x56}, {0x0F, 0x34}, {0x0D, 0x0E}, {0x0B, 0xF5},
	{0x09, 0xD9}, {0x07, 0xB8}, {0x05, 0x8F}, {0x03, 0x5E},
	{0xD1, 0x7E}, {0xCF, 0x76}, {0x01, 0x00}, {0x86, 0x05},
	{0x84, 0x05}, {0x82, 0x05}, {0x80, 0x05}, {0x7E, 0x05},
	{0x7C, 0x05}, {0x7A, 0x05}, {0x78, 0x05}, {0x76, 0x05},
	{0x74, 0x05}, {0x72, 0x05}, {0x70, 0x04}, {0x6E, 0x04},
	{0x6C, 0x04}, {0x6A, 0x04}, {0x68, 0x04}, {0x66, 0x04},
	{0x64, 0x04}, {0x62, 0x03}, {0x60, 0x03}, {0x5E, 0x03},
	{0x5C, 0x03}, {0x5A, 0x03}, {0x58, 0x03}, {0x52, 0x03},
	{0x50, 0x02}, {0x4E, 0x02}, {0x4C, 0x02}, {0x4A, 0x02},
	{0x48, 0x03}, {0xD4, 0x02}, {0xD2, 0x02}, {0x46, 0x00},
	{0x87, 0xAC}, {0x85, 0xA9}, {0x83, 0xA3}, {0x81, 0x96},
	{0x7F, 0x8A}, {0x7D, 0x74}, {0x7B, 0x5E}, {0x79, 0x49},
	{0x77, 0x34}, {0x75, 0x1F}, {0x73, 0x0B}, {0x71, 0xF7},
	{0x6F, 0xE3}, {0x6D, 0xBB}, {0x6B, 0x93}, {0x69, 0x6B},
	{0x67, 0x42}, {0x65, 0x1A}, {0x63, 0xF1}, {0x61, 0xC6},
	{0x5F, 0x91}, {0x5D, 0x76}, {0x5B, 0x5B}, {0x59, 0x39},
	{0x53, 0x12}, {0x51, 0xF9}, {0x4F, 0xDD}, {0x4D, 0xBC},
	{0x4B, 0x93}, {0x49, 0x63}, {0xD5, 0x83}, {0xD3, 0x7B},
	{0x47, 0x00}, {0xCC, 0x06}, {0xCA, 0x06}, {0xC8, 0x06},
	{0xC6, 0x06}, {0xC4, 0x06}, {0xC2, 0x06}, {0xC0, 0x06},
	{0xBE, 0x06}, {0xBC, 0x05}, {0xBA, 0x05}, {0xB8, 0x05},
	{0xB6, 0x05}, {0xB4, 0x05}, {0xB2, 0x05}, {0xB0, 0x05},
	{0xAE, 0x04}, {0xAC, 0x04}, {0xA6, 0x04}, {0xA4, 0x04},
	{0x9E, 0x04}, {0x9C, 0x03}, {0x9A, 0x03}, {0x98, 0x03},
	{0x96, 0x03}, {0x94, 0x03}, {0x92, 0x03}, {0x90, 0x03},
	{0x8E, 0x02}, {0x8C, 0x02}, {0x8A, 0x03}, {0xD8, 0x02},
	{0xD6, 0x02}, {0x88, 0x00}, {0xCD, 0x77}, {0xCB, 0x73},
	{0xC9, 0x6B}, {0xC7, 0x5D}, {0xC5, 0x50}, {0xC3, 0x36},
	{0xC1, 0x1C}, {0xBF, 0x03}, {0xBD, 0xEA}, {0xBB, 0xD1},
	{0xB9, 0xB8}, {0xB7, 0xA0}, {0xB5, 0x87}, {0xB3, 0x56},
	{0xB1, 0x26}, {0xAF, 0xF5}, {0xAD, 0xC4}, {0xA7, 0x92},
	{0xA5, 0x5F}, {0x9F, 0x29}, {0x9D, 0xE6}, {0x9B, 0xC4},
	{0x99, 0xA2}, {0x97, 0x77}, {0x95, 0x48}, {0x93, 0x2B},
	{0x91, 0x0D}, {0x8F, 0xED}, {0x8D, 0xCD}, {0x8B, 0xB1},
	{0xD9, 0xBD}, {0xD7, 0xB6}, {0x89, 0x00}, {0xFE, 0x60},
	{0x00, 0xCC}, {0x01, 0x00}, {0x02, 0x04}, {0x03, 0x00},
	{0x04, 0x00}, {0x05, 0x07}, {0x06, 0x00}, {0x07, 0x88},
	{0x08, 0x00}, {0x09, 0xCC}, {0x0A, 0x00}, {0x0B, 0x04},
	{0x0C, 0x00}, {0x0D, 0x00}, {0x0E, 0x05}, {0x0F, 0x00},
	{0x10, 0x88}, {0x11, 0x00}, {0x12, 0xCC}, {0x13, 0x0F},
	{0x14, 0xFF}, {0x15, 0x04}, {0x16, 0x00}, {0x17, 0x06},
	{0x18, 0x00}, {0x19, 0x96}, {0x1A, 0x00}, {0x24, 0xCC},
	{0x25, 0x00}, {0x26, 0x02}, {0x27, 0x00}, {0x28, 0x00},
	{0x29, 0x06}, {0x2A, 0x06}, {0x2B, 0x82}, {0x2D, 0x00},
	{0x2F, 0xCC}, {0x30, 0x00}, {0x31, 0x02}, {0x32, 0x00},
	{0x33, 0x00}, {0x34, 0x07}, {0x35, 0x06}, {0x36, 0x82},
	{0x37, 0x00}, {0x38, 0xCC}, {0x39, 0x00}, {0x3A, 0x02},
	{0x3B, 0x00}, {0x3D, 0x00}, {0x3F, 0x07}, {0x40, 0x00},
	{0x41, 0x88}, {0x42, 0x00}, {0x43, 0xCC}, {0x44, 0x00},
	{0x45, 0x02}, {0x46, 0x00}, {0x47, 0x00}, {0x48, 0x06},
	{0x49, 0x02}, {0x4A, 0x8A}, {0x4B, 0x00}, {0x5F, 0xCA},
	{0x60, 0x01}, {0x61, 0xE8}, {0x62, 0x09}, {0x63, 0x00},
	{0x64, 0x07}, {0x65, 0x00}, {0x66, 0x30}, {0x67, 0x80},
	{0x9B, 0x03}, {0xA9, 0x07}, {0xAA, 0x06}, {0xAB, 0x02},
	{0xAC, 0x10}, {0xAD, 0x11}, {0xAE, 0x05}, {0xAF, 0x04},
	{0xB0, 0x10}, {0xB1, 0x10}, {0xB2, 0x10}, {0xB3, 0x10},
	{0xB4, 0x10}, {0xB5, 0x10}, {0xB6, 0x10}, {0xB7, 0x10},
	{0xB8, 0x10}, {0xB9, 0x10}, {0xBA, 0x04}, {0xBB, 0x05},
	{0xBC, 0x00}, {0xBD, 0x01}, {0xBE, 0x0A}, {0xBF, 0x10},
	{0xC0, 0x11}, {0xFE, 0xA0}, {0x22, 0x00},
};

static const u32 zh_bus_formats[] = {
	MEDIA_BUS_FMT_RGB888_1X24,
};

static const u32 zh_bus_flags = DRM_BUS_FLAG_DE_LOW |
				DRM_BUS_FLAG_PIXDATA_SAMPLE_POSEDGE;

/* FriendlyElec's dsi_timing1 porches, 1144 x 1948 total, at 132 MHz (59.2 Hz); see the top. */
static const struct drm_display_mode zh_mode = {
	.clock = 132000,
	.hdisplay = 1080,
	.hsync_start = 1080 + 26,
	.hsync_end = 1080 + 26 + 4,
	.htotal = 1080 + 26 + 4 + 34,
	.vdisplay = 1920,
	.vsync_start = 1920 + 8,
	.vsync_end = 1920 + 8 + 4,
	.vtotal = 1920 + 8 + 4 + 16,
	.width_mm = 68,
	.height_mm = 121,
	.flags = DRM_MODE_FLAG_NHSYNC | DRM_MODE_FLAG_NVSYNC,
};

struct zh_panel {
	struct drm_panel panel;
	struct mipi_dsi_device *dsi;

	struct gpio_desc *reset;
	struct backlight_device *backlight;

	struct regulator_bulk_data supplies[2];
	enum drm_panel_orientation orientation;

	bool prepared;
};

static inline struct zh_panel *to_zh_panel(struct drm_panel *panel)
{
	return container_of(panel, struct zh_panel, panel);
}

static int zh_push_cmd_list(struct mipi_dsi_device *dsi)
{
	size_t i;
	int ret;

	for (i = 0; i < ARRAY_SIZE(mcs_zh055oe); i++) {
		u8 buffer[2] = { mcs_zh055oe[i].cmd, mcs_zh055oe[i].param };

		ret = mipi_dsi_generic_write(dsi, buffer, sizeof(buffer));
		if (ret < 0)
			return ret;
	}

	return 0;
}

static int zh_prepare(struct drm_panel *panel)
{
	struct zh_panel *zh = to_zh_panel(panel);
	int ret;

	ret = regulator_bulk_enable(ARRAY_SIZE(zh->supplies), zh->supplies);
	if (ret)
		return ret;

	/* FriendlyElec's sequence: hold reset, at least 10 ms after power-on, release, wait 50 ms. */
	gpiod_set_value_cansleep(zh->reset, 1);
	usleep_range(10000, 12000);
	gpiod_set_value_cansleep(zh->reset, 0);
	msleep(50);

	zh->prepared = true;

	return 0;
}

static int zh_unprepare(struct drm_panel *panel)
{
	struct zh_panel *zh = to_zh_panel(panel);

	/*
	 * Pulse reset and release it, so the touch controller sharing the panel's reset stays
	 * reachable while the display is off.
	 */
	gpiod_set_value_cansleep(zh->reset, 1);
	usleep_range(15000, 17000);
	gpiod_set_value_cansleep(zh->reset, 0);

	zh->prepared = false;

	return regulator_bulk_disable(ARRAY_SIZE(zh->supplies), zh->supplies);
}

static int zh_enable(struct drm_panel *panel)
{
	struct zh_panel *zh = to_zh_panel(panel);
	struct mipi_dsi_device *dsi = zh->dsi;
	struct device *dev = &dsi->dev;
	int ret;

	dsi->mode_flags |= MIPI_DSI_MODE_LPM;

	ret = zh_push_cmd_list(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to send manufacturer commands (%d)\n", ret);
		goto fail;
	}

	/* Select User Command Set table (CMD1) */
	ret = mipi_dsi_generic_write(dsi, (u8[]){ WRMAUCCTR, 0x00 }, 2);
	if (ret < 0)
		goto fail;

	/* DSI mode: video */
	ret = mipi_dsi_generic_write(dsi, (u8[]){ 0xC2, 0x0B }, 2);
	if (ret < 0) {
		dev_err(dev, "Failed to set DSI mode (%d)\n", ret);
		goto fail;
	}

	ret = mipi_dsi_dcs_set_tear_on(dsi, MIPI_DSI_DCS_TEAR_MODE_VBLANK);
	if (ret < 0) {
		dev_err(dev, "Failed to set tear ON (%d)\n", ret);
		goto fail;
	}

	ret = mipi_dsi_dcs_set_tear_scanline(dsi, 0x00);
	if (ret < 0) {
		dev_err(dev, "Failed to set tear scanline (%d)\n", ret);
		goto fail;
	}

	ret = mipi_dsi_dcs_set_pixel_format(dsi, COL_FMT_24BPP);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format (%d)\n", ret);
		goto fail;
	}

	ret = mipi_dsi_dcs_exit_sleep_mode(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to exit sleep mode (%d)\n", ret);
		goto fail;
	}

	/* The panel vendor's delay between sleep-out and display-on. */
	msleep(120);

	ret = mipi_dsi_dcs_set_display_on(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to set display ON (%d)\n", ret);
		goto fail;
	}

	/* And between display-on and brightness. */
	msleep(100);

	backlight_enable(zh->backlight);

	return 0;

fail:
	gpiod_set_value_cansleep(zh->reset, 1);

	return ret;
}

static int zh_disable(struct drm_panel *panel)
{
	struct zh_panel *zh = to_zh_panel(panel);
	struct mipi_dsi_device *dsi = zh->dsi;
	struct device *dev = &dsi->dev;
	int ret;

	dsi->mode_flags |= MIPI_DSI_MODE_LPM;

	backlight_disable(zh->backlight);

	usleep_range(10000, 12000);

	ret = mipi_dsi_dcs_set_display_off(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to set display OFF (%d)\n", ret);
		return ret;
	}

	usleep_range(5000, 10000);

	ret = mipi_dsi_dcs_enter_sleep_mode(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to enter sleep mode (%d)\n", ret);
		return ret;
	}

	return 0;
}

static int zh_get_modes(struct drm_panel *panel, struct drm_connector *connector)
{
	struct drm_display_mode *mode;

	mode = drm_mode_duplicate(connector->dev, &zh_mode);
	if (!mode)
		return -ENOMEM;

	drm_mode_set_name(mode);
	mode->type = DRM_MODE_TYPE_DRIVER | DRM_MODE_TYPE_PREFERRED;
	drm_mode_probed_add(connector, mode);

	connector->display_info.width_mm = mode->width_mm;
	connector->display_info.height_mm = mode->height_mm;
	connector->display_info.bus_flags = zh_bus_flags;

	drm_display_info_set_bus_formats(&connector->display_info, zh_bus_formats,
					 ARRAY_SIZE(zh_bus_formats));

	/*
	 * drm_connector_set_panel_orientation() only takes effect before the device is registered;
	 * .get_orientation below covers the rest.
	 */
	drm_connector_set_panel_orientation(connector, to_zh_panel(panel)->orientation);

	return 1;
}

static enum drm_panel_orientation zh_get_orientation(struct drm_panel *panel)
{
	return to_zh_panel(panel)->orientation;
}

/* An AMOLED panel has no backlight; brightness is the DCS display brightness. */
static int zh_bl_update_status(struct backlight_device *bl)
{
	struct mipi_dsi_device *dsi = bl_get_data(bl);
	struct zh_panel *zh = mipi_dsi_get_drvdata(dsi);
	int ret;

	if (!zh->prepared)
		return 0;

	dsi->mode_flags &= ~MIPI_DSI_MODE_LPM;

	ret = mipi_dsi_dcs_set_display_brightness(dsi, backlight_get_brightness(bl));
	if (ret < 0)
		return ret;

	return 0;
}

static const struct backlight_ops zh_bl_ops = {
	.update_status = zh_bl_update_status,
};

static const struct drm_panel_funcs zh_panel_funcs = {
	.prepare = zh_prepare,
	.unprepare = zh_unprepare,
	.enable = zh_enable,
	.disable = zh_disable,
	.get_modes = zh_get_modes,
	.get_orientation = zh_get_orientation,
};

static int zh_probe(struct mipi_dsi_device *dsi)
{
	struct device *dev = &dsi->dev;
	struct backlight_properties bl_props = {
		.type = BACKLIGHT_RAW,
		.brightness = 255,
		.max_brightness = 255,
	};
	struct zh_panel *zh;
	int ret;

	zh = devm_drm_panel_alloc(dev, struct zh_panel, panel, &zh_panel_funcs,
				  DRM_MODE_CONNECTOR_DSI);
	if (IS_ERR(zh))
		return PTR_ERR(zh);

	mipi_dsi_set_drvdata(dsi, zh);
	zh->dsi = dsi;

	/*
	 * FriendlyElec's settings: four lanes, RGB888, burst video mode, no EoT packet, and
	 * high-speed blanking in the horizontal back porch (MIPI_DSI_MODE_VIDEO_NO_HBP sets the DSI
	 * host's BLK_HBP_HS_EN), which the original Nodo system's DSI host has set too.
	 */
	dsi->format = MIPI_DSI_FMT_RGB888;
	dsi->mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_MODE_VIDEO_BURST |
			  MIPI_DSI_MODE_VIDEO_HSE | MIPI_DSI_MODE_VIDEO_NO_HBP |
			  MIPI_DSI_MODE_NO_EOT_PACKET;
	dsi->lanes = 4;
	of_property_read_u32(dev->of_node, "dsi-lanes", &dsi->lanes);

	zh->reset = devm_gpiod_get(dev, "reset", GPIOD_OUT_LOW);
	if (IS_ERR(zh->reset))
		return dev_err_probe(dev, PTR_ERR(zh->reset), "Failed to get reset GPIO\n");

	/* How the panel sits in the case, from the device tree's rotation property. */
	ret = of_drm_get_panel_orientation(dev->of_node, &zh->orientation);
	if (ret)
		return dev_err_probe(dev, ret, "Invalid rotation property\n");

	zh->supplies[0].supply = "v3p3";
	zh->supplies[1].supply = "v1p8";
	ret = devm_regulator_bulk_get(dev, ARRAY_SIZE(zh->supplies), zh->supplies);
	if (ret)
		return dev_err_probe(dev, ret, "Failed to get supplies\n");

	zh->backlight = devm_backlight_device_register(dev, dev_name(dev), dev, dsi,
						       &zh_bl_ops, &bl_props);
	if (IS_ERR(zh->backlight))
		return dev_err_probe(dev, PTR_ERR(zh->backlight),
				     "Failed to register backlight\n");

	drm_panel_add(&zh->panel);

	ret = mipi_dsi_attach(dsi);
	if (ret) {
		drm_panel_remove(&zh->panel);
		return dev_err_probe(dev, ret, "Failed to attach to the DSI host\n");
	}

	return 0;
}

static void zh_remove(struct mipi_dsi_device *dsi)
{
	struct zh_panel *zh = mipi_dsi_get_drvdata(dsi);
	int ret;

	ret = mipi_dsi_detach(dsi);
	if (ret)
		dev_err(&dsi->dev, "Failed to detach from the DSI host (%d)\n", ret);

	drm_panel_remove(&zh->panel);
}

static const struct of_device_id zh_of_match[] = {
	{ .compatible = "zhot,zh055oe041" },
	{ /* sentinel */ }
};
MODULE_DEVICE_TABLE(of, zh_of_match);

static struct mipi_dsi_driver zh_panel_driver = {
	.driver = {
		.name = "panel-zhot-zh055oe041",
		.of_match_table = zh_of_match,
	},
	.probe = zh_probe,
	.remove = zh_remove,
};
module_mipi_dsi_driver(zh_panel_driver);

MODULE_DESCRIPTION("DRM driver for the ZHOT ZH055OE041 MIPI-DSI AMOLED panel");
MODULE_LICENSE("GPL");
